//! One GraphQL query per repo per 100 shas; everything user-supplied goes in as variables, and nothing is stored.

use std::time::Duration;

use chrono::{DateTime, SecondsFormat};
use serde_json::{json, Value};

pub const SHAS_PER_QUERY: usize = 100;
pub const MAX_HISTORY: usize = 300;
const TIMEOUT: Duration = Duration::from_secs(15);

const COMMIT_FIELDS: &str = "oid additions deletions changedFilesIfAvailable committedDate";
const HISTORY_FIELD: &str = "defaultBranchRef { name target { ... on Commit { history(since: $since, until: $until, first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } nodes { oid committedDate additions deletions changedFilesIfAvailable author { name user { login } } } } } } }";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Repo {
    pub fn url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.name)
    }
}

/// `https://github.com/owner/name` (optionally `.git`) and nothing else.
pub fn parse_repo(url: &str) -> Option<Repo> {
    let (owner, name) = url.strip_prefix("https://github.com/")?.split_once('/')?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    let owner_ok = !owner.is_empty() && owner.len() <= 39 && owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let name_ok = !name.is_empty()
        && name.len() <= 100
        && !name.bytes().all(|b| b == b'.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    (owner_ok && name_ok).then(|| Repo { owner: owner.to_string(), name: name.to_string() })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub since: String,
    pub until: String,
}

impl Window {
    pub fn new(start: f64, end: f64) -> Window {
        let stamp = |t: f64| DateTime::from_timestamp(t.floor() as i64, 0).map_or_else(String::new, |d| d.to_rfc3339_opts(SecondsFormat::Secs, true));
        Window { since: stamp(start), until: stamp(end) }
    }
}

pub struct History<'a> {
    pub window: &'a Window,
    pub cursor: Option<&'a str>,
}

/// The query text only has generated names; owner, name, shas and the window are variables.
pub fn request(repo: &Repo, shas: &[String], history: Option<&History>) -> Value {
    let mut decl = String::from("$owner: String!, $name: String!");
    let mut vars = json!({ "owner": repo.owner, "name": repo.name });
    let mut body = String::new();
    if let Some(h) = history {
        decl.push_str(", $since: GitTimestamp!, $until: GitTimestamp!, $cursor: String");
        vars["since"] = json!(h.window.since);
        vars["until"] = json!(h.window.until);
        vars["cursor"] = json!(h.cursor);
        body.push_str(HISTORY_FIELD);
    }
    for (i, sha) in shas.iter().enumerate() {
        decl.push_str(&format!(", $s{i}: String!"));
        vars[format!("s{i}")] = json!(sha);
        body.push_str(&format!(" c{i}: object(expression: $s{i}) {{ ... on Commit {{ {COMMIT_FIELDS} }} }}"));
    }
    json!({ "query": format!("query({decl}) {{ repository(owner: $owner, name: $name) {{ {body} }} }}"), "variables": vars })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub oid: String,
    pub date: Option<i64>,
    pub added: i64,
    pub removed: i64,
    pub files: Option<i64>,
    pub author: Option<String>,
}

fn commit(v: &Value) -> Option<Commit> {
    let oid = v.get("oid")?.as_str()?;
    if oid.is_empty() || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let text = |p: &str| v.pointer(p).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    Some(Commit {
        oid: oid.to_ascii_lowercase(),
        date: text("/committedDate").and_then(|d| DateTime::parse_from_rfc3339(&d).ok()).map(|d| d.timestamp()),
        added: v.get("additions")?.as_i64()?,
        removed: v.get("deletions")?.as_i64()?,
        files: v.get("changedFilesIfAvailable").and_then(Value::as_i64),
        author: text("/author/user/login").or_else(|| text("/author/name")),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub found: Vec<Option<Commit>>,
    pub history: Vec<Commit>,
    pub next: Option<String>,
}

const UNREADABLE: &str = "not found, or private without access";

/// Only the error type GitHub gave, when it is a plain word, never the body.
fn failure(v: &Value) -> String {
    let errors = v.get("errors").and_then(Value::as_array).filter(|e| !e.is_empty());
    let Some(errors) = errors else { return UNREADABLE.into() };
    match errors[0].get("type").and_then(Value::as_str).filter(|t| !t.is_empty() && t.len() <= 40 && t.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')) {
        Some("NOT_FOUND") => UNREADABLE.into(),
        Some(t) => format!("GitHub error ({t})"),
        None => "GitHub error".into(),
    }
}

pub fn parse(v: &Value, asked: usize) -> Result<Parsed, String> {
    let Some(repo) = v.pointer("/data/repository").filter(|r| r.is_object()) else { return Err(failure(v)) };
    let found = (0..asked).map(|i| repo.get(format!("c{i}")).and_then(commit)).collect();
    let history = repo.pointer("/defaultBranchRef/target/history");
    let nodes = history.and_then(|h| h.get("nodes")).and_then(Value::as_array);
    let next = history
        .filter(|h| h.pointer("/pageInfo/hasNextPage").and_then(Value::as_bool) == Some(true))
        .and_then(|h| h.pointer("/pageInfo/endCursor").and_then(Value::as_str))
        .map(String::from);
    Ok(Parsed { found, history: nodes.map_or_else(Vec::new, |n| n.iter().filter_map(commit).collect()), next })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Match,
    Differ { added: i64, removed: i64 },
    Missing,
}

pub fn verdict(added: i64, removed: i64, found: Option<&Commit>) -> Verdict {
    match found {
        None => Verdict::Missing,
        Some(c) if c.added == added && c.removed == removed => Verdict::Match,
        Some(c) => Verdict::Differ { added: c.added, removed: c.removed },
    }
}

/// Devices store short shas, so this is a prefix match.
pub fn seen(oid: &str, shas: &[String]) -> bool {
    shas.iter().any(|s| !s.is_empty() && oid.starts_with(&s.to_ascii_lowercase()))
}

pub fn unseen<'a>(history: &'a [Commit], shas: &[String]) -> Vec<&'a Commit> {
    history.iter().filter(|c| !seen(&c.oid, shas)).collect()
}

fn status_reason(code: u16) -> String {
    match code {
        401 => "GitHub rejected the token (401)".into(),
        403 | 429 => format!("GitHub refused the request, possibly rate limited ({code})"),
        c => format!("GitHub answered {c}"),
    }
}

pub struct Api<'a> {
    pub http: &'a reqwest::Client,
    pub url: &'a str,
    pub token: &'a str,
}

impl Api<'_> {
    async fn send(&self, body: &Value) -> Result<Value, String> {
        let res = self.http.post(self.url).bearer_auth(self.token).timeout(TIMEOUT).json(body).send().await.map_err(|e| {
            if e.is_timeout() { "GitHub timed out" } else { "couldn't reach GitHub" }.to_string()
        })?;
        if !res.status().is_success() {
            return Err(status_reason(res.status().as_u16()));
        }
        res.json().await.map_err(|_| "GitHub sent something unreadable".to_string())
    }
}

pub struct Fetched {
    pub found: Vec<Option<Commit>>,
    pub history: Vec<Commit>,
    pub more_history: bool,
}

/// Any failure makes the whole repo unreadable.
pub async fn fetch(api: &Api<'_>, repo: &Repo, shas: &[String], window: &Window) -> Result<Fetched, String> {
    let mut chunks = shas.chunks(SHAS_PER_QUERY);
    let first = chunks.next().unwrap_or(&[]);
    let body = request(repo, first, Some(&History { window, cursor: None }));
    let mut page = parse(&api.send(&body).await?, first.len())?;
    let mut found = std::mem::take(&mut page.found);
    let mut history = std::mem::take(&mut page.history);
    while history.len() < MAX_HISTORY {
        let Some(cursor) = page.next.take() else { break };
        let body = request(repo, &[], Some(&History { window, cursor: Some(&cursor) }));
        page = parse(&api.send(&body).await?, 0)?;
        history.append(&mut page.history);
    }
    for chunk in chunks {
        found.extend(parse(&api.send(&request(repo, chunk, None)).await?, chunk.len())?.found);
    }
    Ok(Fetched { found, history, more_history: page.next.is_some() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> Repo {
        Repo { owner: "ada".into(), name: "site".into() }
    }

    fn node(oid: &str, added: i64, removed: i64) -> Value {
        json!({ "oid": oid, "additions": added, "deletions": removed, "changedFilesIfAvailable": 2, "committedDate": "2026-06-02T05:31:00Z",
                "author": { "name": "Ada L", "user": { "login": "ada" } } })
    }

    #[test]
    fn repos_are_parsed_strictly() {
        assert_eq!(parse_repo("https://github.com/obaym/kairos"), Some(Repo { owner: "obaym".into(), name: "kairos".into() }));
        assert_eq!(parse_repo("https://github.com/a-b/c.d_e-f.git").unwrap().name, "c.d_e-f");
        assert_eq!(parse_repo("https://github.com/a/b").unwrap().url(), "https://github.com/a/b");
        for bad in [
            "http://github.com/a/b",
            "https://gitlab.com/a/b",
            "https://github.com.evil.example/a/b",
            "https://github.com/a",
            "https://github.com/a/",
            "https://github.com/a/b/",
            "https://github.com/a/b/tree/main",
            "https://github.com/a/b?x=1",
            "https://github.com/a/b#x",
            "https://github.com/a_b/c",
            "https://github.com//c",
            "https://github.com/a/.git",
            "https://github.com/a/..",
            "https://github.com/a/b c",
            "https://github.com/a/b\"c",
            "https://github.com/ü/b",
            "",
        ] {
            assert_eq!(parse_repo(bad), None, "{bad}");
        }
    }

    #[test]
    fn requests_carry_user_data_as_variables_only() {
        let w = Window::new(1_780_272_000.0, 1_780_358_400.0);
        assert_eq!((w.since.as_str(), w.until.as_str()), ("2026-06-01T00:00:00Z", "2026-06-02T00:00:00Z"));
        let shas = vec!["abc1234".to_string(), "def\") { evil }".to_string()];
        let r = request(&repo(), &shas, Some(&History { window: &w, cursor: None }));
        let q = r["query"].as_str().unwrap();
        assert!(q.contains("history(since: $since, until: $until, first: 100, after: $cursor)"));
        assert!(q.contains("c1: object(expression: $s1)"));
        assert!(q.contains("$s0: String!, $s1: String!"));
        for text in ["ada", "site", "abc1234", "evil", "2026"] {
            assert!(!q.contains(text), "{text} is in the query text: {q}");
        }
        let v = &r["variables"];
        assert_eq!((v["owner"].as_str(), v["name"].as_str()), (Some("ada"), Some("site")));
        assert_eq!((v["s0"].as_str(), v["s1"].as_str()), (Some("abc1234"), Some("def\") { evil }")));
        assert_eq!((v["since"].as_str(), v["until"].as_str(), v["cursor"].is_null()), (Some("2026-06-01T00:00:00Z"), Some("2026-06-02T00:00:00Z"), true));

        let paged = request(&repo(), &[], Some(&History { window: &w, cursor: Some("Y3Vyc29y") }));
        assert_eq!(paged["variables"]["cursor"], "Y3Vyc29y");
        assert!(!paged["query"].as_str().unwrap().contains("object("));

        let plain = request(&repo(), &shas[..1], None);
        assert!(!plain["query"].as_str().unwrap().contains("history"));
        assert!(plain["variables"].get("since").is_none());
    }

    #[test]
    fn responses_are_read_in_the_order_asked() {
        let v = json!({ "data": { "repository": {
            "defaultBranchRef": { "name": "main", "target": { "history": {
                "pageInfo": { "hasNextPage": true, "endCursor": "next" },
                "nodes": [node("aaaa1111", 3, 1), { "oid": "not hex" }, node("BBBB2222", 5, 0)] } } },
            "c0": null,
            "c1": node("cccc3333", 40, 2),
            "c2": {}
        } } });
        let p = parse(&v, 3).unwrap();
        assert_eq!(p.found.len(), 3);
        assert_eq!((p.found[0].is_none(), p.found[2].is_none()), (true, true), "null and non-commit objects are not found");
        let c = p.found[1].as_ref().unwrap();
        assert_eq!((c.oid.as_str(), c.added, c.removed, c.files, c.date), ("cccc3333", 40, 2, Some(2), Some(1_780_378_260)));
        assert_eq!(c.author.as_deref(), Some("ada"));
        assert_eq!(p.history.iter().map(|c| c.oid.as_str()).collect::<Vec<_>>(), ["aaaa1111", "bbbb2222"]);
        assert_eq!(p.next.as_deref(), Some("next"));

        let named = json!({ "oid": "ab12", "additions": 1, "deletions": 0, "author": { "name": "Ada L", "user": null } });
        assert_eq!(commit(&named).unwrap().author.as_deref(), Some("Ada L"));
        assert_eq!(commit(&json!({ "oid": "ab12", "additions": 1 })), None);
    }

    #[test]
    fn empty_repos_and_last_pages_have_no_history_or_cursor() {
        let empty = json!({ "data": { "repository": { "defaultBranchRef": null } } });
        assert_eq!(parse(&empty, 0).unwrap(), Parsed { found: vec![], history: vec![], next: None });
        let last = json!({ "data": { "repository": { "defaultBranchRef": { "target": { "history": { "pageInfo": { "hasNextPage": false, "endCursor": "x" }, "nodes": [] } } } } } });
        assert_eq!(parse(&last, 0).unwrap().next, None);
    }

    #[test]
    fn unreadable_repos_say_why_without_the_body() {
        let gone = json!({ "data": { "repository": null }, "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to a Repository named x" }] });
        assert_eq!(parse(&gone, 1).unwrap_err(), UNREADABLE);
        assert_eq!(parse(&json!({ "data": { "repository": null } }), 1).unwrap_err(), UNREADABLE);
        let limited = json!({ "data": null, "errors": [{ "type": "RATE_LIMITED", "message": "secret-looking text" }] });
        assert_eq!(parse(&limited, 1).unwrap_err(), "GitHub error (RATE_LIMITED)");
        let odd = json!({ "errors": [{ "type": "<script>", "message": "m" }] });
        assert_eq!(parse(&odd, 1).unwrap_err(), "GitHub error");
        assert_eq!(parse(&json!({ "errors": [] }), 1).unwrap_err(), UNREADABLE);
        assert_eq!(parse(&json!("nope"), 1).unwrap_err(), UNREADABLE);
        assert_eq!(status_reason(401), "GitHub rejected the token (401)");
        assert_eq!(status_reason(502), "GitHub answered 502");
    }

    #[test]
    fn commits_are_classified_by_their_line_counts() {
        let c = commit(&node("abcd", 40, 2)).unwrap();
        assert_eq!(verdict(40, 2, Some(&c)), Verdict::Match);
        assert_eq!(verdict(40, 3, Some(&c)), Verdict::Differ { added: 40, removed: 2 });
        assert_eq!(verdict(41, 2, Some(&c)), Verdict::Differ { added: 40, removed: 2 });
        assert_eq!(verdict(40, 2, None), Verdict::Missing);
    }

    #[test]
    fn short_shas_match_by_prefix() {
        let full = "abc1234def5678abc1234def5678abc1234def56";
        assert!(seen(full, &["abc1234".into()]));
        assert!(seen(full, &["ABC1234".into()]));
        assert!(seen(full, &["zzz9999".into(), full.into()]));
        assert!(!seen(full, &["abc1235".into()]));
        assert!(!seen(full, &["bc1234".into()]));
        assert!(!seen(full, &[]));
        assert!(!seen(full, &[String::new()]));
        let history: Vec<Commit> = [full, "ffff0000aaaa1111ffff0000aaaa1111ffff0000"].iter().map(|o| commit(&node(o, 1, 1)).unwrap()).collect();
        let left = unseen(&history, &["abc1234".into()]);
        assert_eq!(left.len(), 1);
        assert!(left[0].oid.starts_with("ffff0000"));
    }
}
