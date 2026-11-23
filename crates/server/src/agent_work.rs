//! Windows are per file, since a project can have several working copies on one device (worktrees, clones).
//! Windows use when the device saw each commit, never git's dates, which a committer can set to move agent lines out of a window.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use deadpool_postgres::GenericClient;

use crate::aliases::canon_commits;
use crate::util::Res;

const FIRST_WINDOW: f64 = 7.0 * 86_400.0;
const QUIET_MATCH: f64 = 60.0;
const COMMIT_SLACK: f64 = 3_600.0;

pub struct Commit<'a> {
    pub device: Option<i64>,
    pub project: &'a str,
    pub sha: &'a str,
    pub ts: f64,
    pub committed: Option<i64>,
    pub via: Option<&'a str>,
    pub files: Vec<(&'a str, i64, i64)>,
}

impl Commit<'_> {
    /// Pulled, rebased, cherry-picked and reverted commits carry changes written before.
    fn authored_here(&self) -> bool {
        self.device.is_some() && matches!(self.via, None | Some("commit" | "amend" | "unknown"))
    }
}

pub struct Local {
    pub device: i64,
    pub project: String,
    pub ts: f64,
    pub committed: Option<i64>,
    pub amend: bool,
    pub paths: Vec<String>,
}

pub struct Edit {
    pub device: i64,
    pub project: String,
    pub ts: f64,
    pub path: String,
    pub added: i64,
    pub removed: i64,
    pub tool: Option<String>,
}

pub struct AgentCommit {
    pub device: i64,
    pub ts: f64,
    pub project: Option<String>,
    pub sha: Option<String>,
    pub tool: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Work {
    pub compared: bool,
    pub files: HashMap<String, (i64, i64)>,
    /// When each compared file's window opens: its previous commit on the device, or a week back.
    pub since: HashMap<String, f64>,
    pub added: i64,
    pub removed: i64,
    pub tools: Vec<String>,
    pub made_by: Option<String>,
}

pub async fn load(c: &impl GenericClient, user_id: i64, commits: &[Commit<'_>]) -> Res<Vec<Work>> {
    let compared: Vec<&Commit> = commits.iter().filter(|c| c.authored_here() && !c.files.is_empty()).collect();
    let (mut locals, mut edits) = (Vec::new(), Vec::new());
    if let Some(lo) = compared.iter().map(|c| c.ts).reduce(f64::min) {
        let hi = compared.iter().map(|c| c.ts).fold(lo, f64::max);
        let devices: Vec<i64> = compared.iter().filter_map(|c| c.device).collect::<BTreeSet<_>>().into_iter().collect();
        let projects: Vec<&str> = compared.iter().map(|c| c.project).collect::<BTreeSet<_>>().into_iter().collect();
        let paths: Vec<String> =
            compared.iter().flat_map(|c| c.files.iter().map(|f| f.0.to_lowercase())).collect::<BTreeSet<_>>().into_iter().collect();
        let from = lo - FIRST_WINDOW;
        locals = c
            .query(
                concat!(
                    "SELECT device_id, project, ts, committed_at, via = 'amend', COALESCE(jsonb_path_query_array(changes, '$[*].path'), '[]')
                     FROM ",
                    canon_commits!(),
                    " c WHERE device_id = ANY($2) AND project = ANY($3) AND ts >= $4 AND ts <= $5
                       AND via IS DISTINCT FROM 'arrived'"
                ),
                &[&user_id, &devices, &projects, &from, &hi],
            )
            .await?
            .iter()
            .map(|r| Local {
                device: r.get(0),
                project: r.get(1),
                ts: r.get(2),
                committed: r.get(3),
                amend: r.get::<_, Option<bool>>(4).unwrap_or(false),
                paths: serde_json::from_value(r.get(5)).unwrap_or_default(),
            })
            .collect();
        let count = |field: &str| {
            format!(
                "CASE WHEN jsonb_typeof(e.body->'{field}') = 'number' THEN LEAST(GREATEST((e.body->>'{field}')::numeric, 0), 4294967295)::int8 ELSE 0 END"
            )
        };
        edits = c
            .query(
                &format!(
                    "SELECT e.device_id, COALESCE(a.canonical, e.project), e.ts, e.body->>'path', {}, {},
                            CASE WHEN jsonb_typeof(e.body->'harness') = 'string' THEN e.body->>'harness' END
                     FROM events e LEFT JOIN project_aliases a ON a.user_id = $6 AND a.alias = e.project
                     WHERE e.device_id = ANY($1) AND e.kind = 'agent_edit' AND e.ts > $2 AND e.ts <= $3 AND COALESCE(a.canonical, e.project) = ANY($4)
                       AND jsonb_typeof(e.body->'path') = 'string' AND lower(e.body->>'path') = ANY($5)",
                    count("added"),
                    count("removed")
                ),
                &[&devices, &from, &hi, &projects, &paths, &user_id],
            )
            .await?
            .iter()
            .map(|r| Edit { device: r.get(0), project: r.get(1), ts: r.get(2), path: r.get(3), added: r.get(4), removed: r.get(5), tool: r.get(6) })
            .collect();
    }
    let agent_commits = match commits.iter().map(|c| c.ts).reduce(f64::min) {
        None => Vec::new(),
        Some(lo) => c
            .query(
                "SELECT e.device_id, e.ts, COALESCE(a.canonical, e.project),
                        CASE WHEN jsonb_typeof(e.body->'sha') = 'string' THEN e.body->>'sha' END,
                        CASE WHEN jsonb_typeof(e.body->'harness') = 'string' THEN e.body->>'harness' END,
                        e.body ? 'sha'
                 FROM events e LEFT JOIN project_aliases a ON a.user_id = $1 AND a.alias = e.project
                 WHERE e.device_id IN (SELECT id FROM devices WHERE user_id = $1) AND e.kind = 'agent_commit' AND e.ts >= $2 AND e.ts <= $3
                 ORDER BY e.ts",
                &[&user_id, &(lo - COMMIT_SLACK), &(commits.iter().map(|c| c.ts).fold(lo, f64::max) + COMMIT_SLACK)],
            )
            .await?
            .iter()
            // A sha that isn't a string is a broken event, not a quiet commit.
            .filter(|r| r.get::<_, Option<String>>(3).is_some() || !r.get::<_, bool>(5))
            .map(|r| AgentCommit { device: r.get(0), ts: r.get(1), project: r.get(2), sha: r.get(3), tool: r.get(4) })
            .collect(),
    };
    let mut work = attribute(commits, &locals, &edits);
    made_by(commits, &agent_commits, &mut work);
    Ok(work)
}

fn order(ts: f64, committed: Option<i64>) -> (f64, i64) {
    (ts, committed.unwrap_or(i64::MIN))
}

fn earlier(a: (f64, Option<i64>), b: (f64, Option<i64>)) -> bool {
    order(a.0, a.1).partial_cmp(&order(b.0, b.1)) == Some(Ordering::Less)
}

/// A path matches exactly, else ignoring case (Windows paths can differ in case only).
pub fn attribute(commits: &[Commit<'_>], locals: &[Local], edits: &[Edit]) -> Vec<Work> {
    let mut by_repo: HashMap<(i64, &str), Vec<&Local>> = HashMap::new();
    for l in locals {
        by_repo.entry((l.device, l.project.as_str())).or_default().push(l);
    }
    let mut by_path: HashMap<(i64, &str, String), Vec<&Edit>> = HashMap::new();
    for e in edits {
        by_path.entry((e.device, e.project.as_str(), e.path.to_lowercase())).or_default().push(e);
    }
    commits
        .iter()
        .map(|c| {
            let mut w = Work::default();
            let Some(device) = c.device.filter(|_| c.authored_here() && !c.files.is_empty()) else { return w };
            w.compared = true;
            let me = (c.ts, c.committed);
            let before: Vec<&Local> =
                by_repo.get(&(device, c.project)).into_iter().flatten().copied().filter(|l| earlier((l.ts, l.committed), me)).collect();
            let last_touch = |path: &str, until: (f64, Option<i64>), plain: bool| {
                before
                    .iter()
                    .filter(|l| earlier((l.ts, l.committed), until) && !(plain && l.amend) && l.paths.iter().any(|p| p == path))
                    .max_by(|a, b| order(a.ts, a.committed).partial_cmp(&order(b.ts, b.committed)).unwrap_or(Ordering::Equal))
                    .map(|l| (l.ts, l.committed))
            };
            let mut tools = BTreeSet::new();
            for &(path, added, removed) in &c.files {
                let start = if c.via == Some("amend") {
                    last_touch(path, me, true).and_then(|amended| last_touch(path, amended, false))
                } else {
                    last_touch(path, me, false)
                };
                let lo = start.map_or(c.ts - FIRST_WINDOW, |(t, _)| t.max(c.ts - FIRST_WINDOW));
                w.since.insert(path.to_string(), lo);
                let inside: Vec<&Edit> = by_path
                    .get(&(device, c.project, path.to_lowercase()))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|e| e.ts > lo && e.ts <= c.ts)
                    .collect();
                let exact: Vec<&Edit> = inside.iter().copied().filter(|e| e.path == path).collect();
                let used = if exact.is_empty() { inside } else { exact };
                let (a, r) = used.iter().fold((0, 0), |(a, r), e| (a + e.added, r + e.removed));
                let lines = (a.min(added.max(0)), r.min(removed.max(0)));
                if lines == (0, 0) {
                    continue;
                }
                tools.extend(used.iter().filter_map(|e| e.tool.as_deref()).filter(|t| !t.is_empty()));
                w.added += lines.0;
                w.removed += lines.1;
                w.files.insert(path.to_string(), lines);
            }
            w.tools = tools.into_iter().map(String::from).collect();
            w
        })
        .collect()
}

fn valid_sha(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn made_by(commits: &[Commit<'_>], agent: &[AgentCommit], work: &mut [Work]) {
    for a in agent {
        let tool = a.tool.clone().unwrap_or_default();
        let hit = match &a.sha {
            Some(sha) if valid_sha(sha) => commits.iter().position(|c| c.sha.starts_with(sha.as_str())),
            Some(_) => None,
            None => commits
                .iter()
                .enumerate()
                .filter(|(i, c)| {
                    work[*i].made_by.is_none()
                        && c.device == Some(a.device)
                        && a.project.as_deref() == Some(c.project)
                        && matches!(c.via, None | Some("commit" | "amend" | "unknown"))
                        && (c.ts - a.ts).abs() <= QUIET_MATCH
                })
                .min_by(|(_, x), (_, y)| (x.ts - a.ts).abs().total_cmp(&(y.ts - a.ts).abs()))
                .map(|(i, _)| i),
        };
        if let Some(i) = hit {
            work[i].made_by.get_or_insert(tool);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit<'a>(sha: &'a str, ts: f64, via: Option<&'a str>, files: Vec<(&'a str, i64, i64)>) -> Commit<'a> {
        Commit { device: Some(1), project: "o/r", sha, ts, committed: Some(ts as i64), via, files }
    }

    fn local(ts: f64, amend: bool, paths: &[&str]) -> Local {
        Local { device: 1, project: "o/r".into(), ts, committed: Some(ts as i64), amend, paths: paths.iter().map(|p| p.to_string()).collect() }
    }

    fn edit(ts: f64, path: &str, added: i64, removed: i64, tool: &str) -> Edit {
        Edit { device: 1, project: "o/r".into(), ts, path: path.into(), added, removed, tool: Some(tool.into()) }
    }

    #[test]
    fn agent_lines_never_exceed_the_commit_and_unedited_files_count_none() {
        let commits = [commit("a", 100.0, Some("commit"), vec![("src/a.rs", 10, 4), ("src/b.rs", 3, 0), ("Cargo.lock", 40, 2)])];
        let edits = [
            edit(10.0, "src/a.rs", 25, 1, "claude-code"),
            edit(11.0, "SRC/B.rs", 2, 0, "codex"),
            edit(12.0, "src/b.rs", 1, 0, "claude-code"),
            edit(13.0, "notes/plan.md", 9, 0, "claude-code"),
        ];
        let w = &attribute(&commits, &[], &edits)[0];
        assert!(w.compared);
        assert_eq!(w.files.get("src/a.rs"), Some(&(10, 1)), "capped at the commit's own lines");
        assert_eq!(w.files.get("src/b.rs"), Some(&(1, 0)), "an exact path wins over one differing in case");
        assert!(!w.files.contains_key("Cargo.lock") && !w.files.contains_key("notes/plan.md"));
        assert_eq!((w.added, w.removed), (11, 1));
        assert_eq!(w.tools, ["claude-code"]);
    }

    #[test]
    fn a_path_differing_only_in_case_still_matches() {
        let commits = [commit("a", 100.0, None, vec![("Src/Main.rs", 5, 5)])];
        let w = &attribute(&commits, &[], &[edit(50.0, "src/main.rs", 3, 9, "codex")])[0];
        assert_eq!((w.added, w.removed, w.tools.clone()), (3, 5, vec!["codex".to_string()]));
    }

    #[test]
    fn a_commit_elsewhere_in_the_project_does_not_cut_a_files_window() {
        // A worktree's commit at 150 touched another file; a.rs was last committed at 50.
        let commits = [commit("c", 200.0, Some("commit"), vec![("a.rs", 30, 0)])];
        let locals = [local(50.0, false, &["a.rs"]), local(150.0, false, &["probe.rs"]), local(200.0, false, &["a.rs"])];
        let edits = [edit(40.0, "a.rs", 7, 0, "codex"), edit(120.0, "a.rs", 20, 0, "codex"), edit(201.0, "a.rs", 5, 0, "codex")];
        let w = &attribute(&commits, &locals, &edits)[0];
        assert_eq!((w.added, w.removed), (20, 0), "after the last commit touching a.rs, up to when this one was seen");
        assert_eq!(w.since.get("a.rs"), Some(&50.0));
    }

    #[test]
    fn an_amend_counts_from_before_the_commit_it_amended() {
        let commits = [commit("b2", 300.0, Some("amend"), vec![("a.rs", 9, 0)])];
        let locals = [local(100.0, false, &["a.rs"]), local(200.0, false, &["a.rs"]), local(250.0, true, &["a.rs"]), local(300.0, true, &["a.rs"])];
        let edits = [edit(90.0, "a.rs", 1, 0, "codex"), edit(150.0, "a.rs", 4, 0, "codex"), edit(260.0, "a.rs", 3, 0, "codex")];
        let w = &attribute(&commits, &locals, &edits)[0];
        assert_eq!(w.added, 7, "edits since 100: the chain 200, 250, 300 is one commit");
    }

    #[test]
    fn commits_seen_together_split_by_their_commit_time() {
        let mut first = commit("a", 100.0, Some("commit"), vec![("a.rs", 5, 0)]);
        first.committed = Some(90);
        let mut second = commit("b", 100.0, Some("commit"), vec![("a.rs", 5, 0)]);
        second.committed = Some(95);
        let mut l1 = local(100.0, false, &["a.rs"]);
        l1.committed = Some(90);
        let mut l2 = local(100.0, false, &["a.rs"]);
        l2.committed = Some(95);
        let work = attribute(&[first, second], &[l1, l2], &[edit(80.0, "a.rs", 5, 0, "codex")]);
        assert_eq!((work[0].added, work[1].added), (5, 0), "the earlier commit has the edits, the later one an empty window");
    }

    #[test]
    fn pulled_commits_and_commits_without_files_are_not_compared() {
        let commits = [commit("a", 100.0, Some("arrived"), vec![("a.rs", 5, 0)]), commit("b", 100.0, Some("commit"), vec![])];
        let work = attribute(&commits, &[], &[edit(50.0, "a.rs", 5, 0, "codex")]);
        assert!(work.iter().all(|w| !w.compared && w.added == 0));
    }

    fn agent(ts: f64, sha: Option<&str>, project: &str) -> AgentCommit {
        AgentCommit { device: 1, ts, project: Some(project.into()), sha: sha.map(String::from), tool: Some("claude-code".into()) }
    }

    #[test]
    fn agent_commits_match_by_sha_prefix_or_by_time_on_the_same_device_and_project() {
        let commits = [
            commit("aaaaaaa111", 1000.0, Some("commit"), vec![]),
            commit("bbbbbbb222", 2000.0, Some("commit"), vec![]),
            commit("ccccccc333", 2030.0, Some("commit"), vec![]),
            commit("ddddddd444", 3000.0, Some("arrived"), vec![]),
            commit("eeeeeee555", 4000.0, Some("commit"), vec![]),
        ];
        let mut work: Vec<Work> = commits.iter().map(|_| Work::default()).collect();
        made_by(
            &commits,
            &[
                agent(990.0, Some("aaaaaaa"), "other/project"),
                agent(2020.0, None, "o/r"),
                agent(2021.0, None, "o/r"),
                agent(3001.0, None, "o/r"),
                agent(4000.0, None, "x/y"),
                agent(4000.0, Some("EEEEEEE"), "o/r"),
                agent(4000.0, Some("eeee"), "o/r"),
            ],
            &mut work,
        );
        let made: Vec<Option<&str>> = work.iter().map(|w| w.made_by.as_deref()).collect();
        assert_eq!(made, [Some("claude-code"), Some("claude-code"), Some("claude-code"), None, None]);
    }

    #[test]
    fn a_quiet_agent_commit_too_far_from_any_commit_marks_none() {
        let commits = [commit("aaaaaaa", 1000.0, Some("commit"), vec![])];
        let mut work = vec![Work::default()];
        made_by(&commits, &[agent(1000.0 + QUIET_MATCH + 1.0, None, "o/r")], &mut work);
        assert_eq!(work[0].made_by, None);
    }
}
