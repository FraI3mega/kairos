use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use super::Found;
use crate::event::Kind;

// Only paths and counts come out of here, never text.

const MAX_DIFF_LINES: usize = 20_000;
const MAX_PENDING: usize = 2_000;

pub(crate) fn line_count(s: &str) -> u32 {
    s.lines().count().try_into().unwrap_or(u32::MAX)
}

pub(crate) fn diff_counts(old: &str, new: &str) -> Option<(u32, u32)> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (n, m) = (a.len(), b.len());
    if n + m > MAX_DIFF_LINES {
        return None;
    }
    if n == 0 || m == 0 {
        return Some((m as u32, n as u32));
    }
    // Myers' shortest edit script length, forward pass only: the counts follow from it and from n - m.
    let max = n + m;
    let off = max as isize;
    let mut v = vec![0isize; 2 * max + 2];
    for d in 0..=max as isize {
        if d as usize > MAX_DIFF_LINES / 10 {
            return None;
        }
        let mut k = -d;
        while k <= d {
            let i = (off + k) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) { v[i + 1] } else { v[i - 1] + 1 };
            let mut y = x - k;
            while (x as usize) < n && (y as usize) < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x as usize >= n && y as usize >= m {
                let (added, removed) = ((d + m as isize - n as isize) / 2, (d - m as isize + n as isize) / 2);
                return Some((added as u32, removed as u32));
            }
            k += 2;
        }
    }
    None
}

// `\ No newline` markers and anything before the first hunk header are not lines.
pub(crate) fn patch_counts(patch: &str) -> (u32, u32) {
    hunk_counts(patch.lines().skip_while(|l| !l.starts_with("@@")))
}

pub(crate) fn hunk_counts<'a>(lines: impl IntoIterator<Item = &'a str>) -> (u32, u32) {
    let (mut added, mut removed) = (0u32, 0u32);
    for l in lines {
        match l.as_bytes().first() {
            Some(b'+') => added = added.saturating_add(1),
            Some(b'-') => removed = removed.saturating_add(1),
            _ => {}
        }
    }
    (added, removed)
}

// Deleted files are left out: the patch does not say how long they were.
pub(crate) fn envelope_counts(patch: &str) -> Vec<(String, u32, u32)> {
    let mut files: Vec<(String, u32, u32)> = Vec::new();
    let mut cur: Option<(String, u32, u32)> = None;
    for line in patch.lines() {
        let section = |m: &str| line.trim_end().strip_prefix(m).map(|p| p.trim().to_string());
        if let Some(p) = section("*** Add File: ").or_else(|| section("*** Update File: ")) {
            files.extend(cur.take());
            cur = Some((p, 0, 0));
        } else if section("*** Delete File: ").is_some() || line.starts_with("*** End Patch") || line.starts_with("*** Begin Patch") {
            files.extend(cur.take());
        } else if let Some(p) = section("*** Move to: ") {
            if let Some(c) = cur.as_mut() {
                c.0 = p;
            }
        } else if let Some(c) = cur.as_mut() {
            match line.as_bytes().first() {
                Some(b'+') => c.1 = c.1.saturating_add(1),
                Some(b'-') => c.2 = c.2.saturating_add(1),
                _ => {}
            }
        }
    }
    files.extend(cur);
    files
}

// Lines changed by a Gemini CLI / Qwen Code edit tool, from its `resultDisplay`: the unified diff of
// what was written, else the before and after text, else the model's own diff stat when the user did
// not alter the proposal on top of it.
pub(crate) fn display_counts(rd: &serde_json::Value) -> Option<(u32, u32)> {
    let text = |k: &str| rd.get(k).and_then(serde_json::Value::as_str);
    if let Some(d) = text("fileDiff") {
        return Some(patch_counts(d));
    }
    if let Some(after) = text("newContent") {
        return diff_counts(text("originalContent").unwrap_or(""), after);
    }
    let stat = rd.get("diffStat")?;
    let n = |k: &str| stat.get(k).and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok());
    if n("user_added_lines").unwrap_or(0) + n("user_removed_lines").unwrap_or(0) > 0 {
        return None;
    }
    Some((n("model_added_lines")?, n("model_removed_lines")?))
}

// Nothing is reported for a call that changed no lines.
pub(crate) fn edit(ts: f64, cwd: Option<&Path>, session: &str, path: &str, (added, removed): (u32, u32), out: &mut Vec<Found>) {
    if path.is_empty() || (added == 0 && removed == 0) {
        return;
    }
    out.push(Found {
        ts,
        cwd: cwd.map(Path::to_path_buf),
        kind: Kind::AgentEdit { session: session.to_string(), path: path.to_string(), added, removed },
    });
}

// A command that printed nothing at all (`git commit -q`, or a harness that keeps no output) gives one without a sha per commit
// it ran; anything else printed without a sha means no commit was made.
pub(crate) fn commits(ts: f64, cwd: Option<&Path>, session: &str, command: &str, output: &str, out: &mut Vec<Found>) {
    let runs = commit_runs(command);
    if runs.is_empty() {
        return;
    }
    let at = |dir: &Option<String>| match (cwd, dir) {
        (Some(c), Some(d)) => Some(c.join(d)),
        (None, Some(d)) if Path::new(d).is_absolute() => Some(PathBuf::from(d)),
        (c, _) => c.map(Path::to_path_buf),
    };
    let commit = |dir: &Option<String>, sha: Option<String>| Found {
        ts,
        cwd: at(dir),
        kind: Kind::AgentCommit { session: session.to_string(), sha },
    };
    if output.trim().is_empty() {
        out.extend(runs.iter().map(|dir| commit(dir, None)));
        return;
    }
    let shas = printed_shas(output);
    for (i, sha) in shas.iter().enumerate() {
        let dir = if runs.len() == shas.len() { &runs[i] } else { &runs[0] };
        out.push(commit(dir, Some(sha.clone())));
    }
}

// `[main abc1234] msg`, `[main (root-commit) abc1234] msg`, `[detached HEAD abc1234] msg`.
fn printed_shas(output: &str) -> Vec<String> {
    let mut shas: Vec<String> = Vec::new();
    for line in output.lines() {
        let Some(inner) = line.trim_start().strip_prefix('[').and_then(|r| r.split_once(']')).map(|(i, _)| i) else { continue };
        let words: Vec<&str> = inner.split_whitespace().collect();
        let Some((sha, head)) = words.split_last() else { continue };
        let head_ok = match head {
            [_] | [_, "(root-commit)"] | ["detached", "HEAD"] | ["detached", "HEAD", "(root-commit)"] => true,
            _ => false,
        };
        let hex = (7..=64).contains(&sha.len()) && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if head_ok && hex && !shas.iter().any(|s| s == sha) {
            shas.push(sha.to_string());
        }
    }
    shas
}

fn commit_runs(command: &str) -> Vec<Option<String>> {
    let mut runs = Vec::new();
    let mut dir: Option<String> = None;
    scan(command, 0, &mut dir, &mut runs, &mut false);
    runs
}

// Git subcommands that put lines on disk which this command did not write.
pub(crate) fn restores_files(command: &str) -> bool {
    let mut restores = false;
    scan(command, 0, &mut None, &mut Vec::new(), &mut restores);
    restores
}

fn scan(script: &str, depth: usize, dir: &mut Option<String>, runs: &mut Vec<Option<String>>, restores: &mut bool) {
    for seg in segments(script) {
        let mut w: &[String] = &seg;
        while let Some(first) = w.first() {
            let var = first.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
            if var || matches!(first.as_str(), "sudo" | "env" | "command" | "time" | "nohup" | "exec" | "builtin") {
                w = &w[1..];
            } else {
                break;
            }
        }
        let Some(name) = w.first().map(|f| program(f)) else { continue };
        match name {
            "cd" | "pushd" => {
                if let Some(d) = w.get(1).filter(|d| !d.starts_with('-')) {
                    *dir = Some(join(dir.as_deref(), d));
                }
            }
            "git" => {
                let mut at = dir.clone();
                let mut i = 1;
                while i < w.len() {
                    match w[i].as_str() {
                        "-C" => {
                            if let Some(d) = w.get(i + 1) {
                                at = Some(join(at.as_deref(), d));
                            }
                            i += 2;
                        }
                        "-c" | "--namespace" | "--exec-path" | "--super-prefix" | "--config-env" => i += 2,
                        o if o.starts_with('-') => i += 1,
                        _ => break,
                    }
                }
                match w.get(i).map(String::as_str) {
                    Some("commit") => runs.push(at),
                    Some(
                        "stash" | "checkout" | "restore" | "reset" | "pull" | "merge" | "rebase" | "cherry-pick" | "revert" | "switch" | "am",
                    ) => *restores = true,
                    _ => {}
                }
            }
            "bash" | "sh" | "zsh" | "dash" | "ksh" | "pwsh" | "powershell" if depth < 3 => {
                let script = w.iter().skip(1).position(|a| {
                    let a = a.to_ascii_lowercase();
                    a == "-command" || a == "-c" || (a.starts_with('-') && !a.starts_with("--") && a.ends_with('c') && a.len() <= 4)
                });
                if let Some(s) = script.and_then(|p| w.get(p + 2)) {
                    let mut inner = dir.clone();
                    scan(s, depth + 1, &mut inner, runs, restores);
                }
            }
            _ => {}
        }
    }
}

fn program(w: &str) -> &str {
    let base = w.rsplit(['/', '\\']).next().unwrap_or(w);
    base.strip_suffix(".exe").unwrap_or(base)
}

fn join(base: Option<&str>, d: &str) -> String {
    let abs = d.starts_with('/') || d.starts_with('\\') || d.as_bytes().get(1) == Some(&b':') || d.starts_with('~');
    match base {
        Some(b) if !abs => format!("{}/{d}", b.trim_end_matches('/')),
        _ => d.to_string(),
    }
}

// Quoted text (commit messages) stays inside one word, so nothing in it can pass for a command. Not a shell: no expansion, no heredocs.
fn segments(s: &str) -> Vec<Vec<String>> {
    let mut segs: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut live = false;
    let mut chars: VecDeque<char> = s.chars().collect();
    macro_rules! word {
        () => {
            if live {
                words.push(std::mem::take(&mut cur));
                live = false;
            }
        };
    }
    macro_rules! seg {
        () => {
            word!();
            if !words.is_empty() {
                segs.push(std::mem::take(&mut words));
            }
        };
    }
    while let Some(c) = chars.pop_front() {
        match c {
            '\'' => {
                live = true;
                while let Some(q) = chars.pop_front() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                live = true;
                while let Some(q) = chars.pop_front() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.front(), Some('"' | '\\' | '$' | '`')) => cur.extend(chars.pop_front()),
                        _ => cur.push(q),
                    }
                }
            }
            '\\' => {
                live = true;
                if let Some(n) = chars.pop_front() {
                    if n != '\n' {
                        cur.push(n);
                    }
                }
            }
            ' ' | '\t' | '\r' => word!(),
            '\n' | ';' | '&' | '|' | '(' | ')' | '{' | '}' => {
                seg!();
            }
            c => {
                live = true;
                cur.push(c);
            }
        }
    }
    if live {
        words.push(cur);
    }
    if !words.is_empty() {
        segs.push(words);
    }
    segs
}

// An argv as one shell line, so quoted arguments stay single words when it is scanned again.
pub(crate) fn shell_join<'a>(args: impl IntoIterator<Item = &'a str>) -> String {
    let quote = |a: &str| {
        let plain = !a.is_empty() && a.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&b));
        if plain {
            a.to_string()
        } else {
            format!("'{}'", a.replace('\'', r"'\''"))
        }
    };
    args.into_iter().map(quote).collect::<Vec<_>>().join(" ")
}

// Calls waiting for their result; a log that never delivers them can't grow this without bound.
pub(crate) fn remember<T>(map: &mut std::collections::HashMap<String, T>, id: String, v: T) {
    if map.len() >= MAX_PENDING {
        map.clear();
    }
    map.insert(id, v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_diffs_count_only_changed_lines() {
        assert_eq!(diff_counts("a\nb\nc\n", "a\nx\nc\n"), Some((1, 1)));
        assert_eq!(diff_counts("a\nb\n", "a\nb\nc\nd\n"), Some((2, 0)));
        assert_eq!(diff_counts("a\nb\nc\nd\n", "d\n"), Some((0, 3)));
        assert_eq!(diff_counts("", "a\nb\n"), Some((2, 0)));
        assert_eq!(diff_counts("a\nb\nc\nd\ne\n", "b\nc\nx\ne\nf\n"), Some((2, 2)));
        assert_eq!(diff_counts("same\n", "same\n"), Some((0, 0)));
        let big_a = "a\n".repeat(MAX_DIFF_LINES);
        assert_eq!(diff_counts(&big_a, &"b\n".repeat(5)), None);
    }

    #[test]
    fn unified_patches_skip_headers_and_markers() {
        let p = "Index: f\n===\n--- f\n+++ f\n@@ -1,3 +1,3 @@\n a\n-b\n+c\n+d\n\\ No newline at end of file\n@@ -9 +9 @@\n--x\n+++y\n";
        assert_eq!(patch_counts(p), (3, 2));
        assert_eq!(patch_counts("@@ -1 +1 @@\n-a\n+b\n"), (1, 1));
        assert_eq!(patch_counts("no hunks here\n+x\n"), (0, 0));
    }

    #[test]
    fn apply_patch_envelopes_count_per_file_and_skip_deletes() {
        let p = "*** Begin Patch\n*** Update File: a.rs\n*** Move to: b.rs\n@@ fn x\n keep\n-old\n+new\n+more\n*** Add File: c.rs\n+1\n+2\n*** Delete File: d.rs\n*** Update File: e.rs\n@@\n-gone\n*** End Patch";
        assert_eq!(
            envelope_counts(p),
            [("b.rs".to_string(), 2, 1), ("c.rs".to_string(), 2, 0), ("e.rs".to_string(), 0, 1)]
        );
        assert!(envelope_counts("no patch here\n+x\n").is_empty());
    }

    fn shas(command: &str, output: &str) -> Vec<(Option<String>, Option<String>)> {
        let mut out = Vec::new();
        commits(1.0, Some(Path::new("/w/p")), "s", command, output, &mut out);
        out.into_iter()
            .map(|f| match f.kind {
                Kind::AgentCommit { sha, .. } => (sha, f.cwd.map(|c| c.to_string_lossy().replace('\\', "/"))),
                k => panic!("{k:?}"),
            })
            .collect()
    }

    #[test]
    fn commits_need_the_command_and_a_printed_sha() {
        let printed = "[main abc1234] add the thing\n 1 file changed, 2 insertions(+)\n";
        let at = |d: &str| Some(d.to_string());
        assert_eq!(shas("git commit -m \"add the thing\"", printed), [(Some("abc1234".into()), at("/w/p"))]);
        assert_eq!(shas("git add -A && git commit -am x", printed).len(), 1);
        assert_eq!(shas("git -C sub commit -m x", printed), [(Some("abc1234".into()), at("/w/p/sub"))]);
        assert_eq!(shas("cd /w/q && git commit -m x; git status", printed), [(Some("abc1234".into()), at("/w/q"))]);
        assert_eq!(shas("cd sub && git -C inner -c core.x=1 commit -m x", printed), [(Some("abc1234".into()), at("/w/p/sub/inner"))]);
        assert_eq!(shas("FOO=1 sudo git --no-pager commit --amend --no-edit", printed).len(), 1);
        assert_eq!(shas("bash -lc 'git commit -m x'", printed).len(), 1);
        assert_eq!(shas("git commit -m \"$(cat <<'EOF'\nsubject\n\nbody; git push\nEOF\n)\"", printed).len(), 1);
        for root in ["[main (root-commit) abc1234] first\n", "[detached HEAD abc1234] x\n", "[feat/x-1 ABCDEF1234567] x\n"] {
            let n = shas("git commit -m x", root).len();
            assert_eq!(n, usize::from(!root.contains("ABCDEF")), "{root}");
        }
        assert_eq!(shas("git commit -m a && git commit -m b", "[main aaaaaaa] a\n[main bbbbbbb] b\n").len(), 2);
    }

    #[test]
    fn other_commands_and_failed_commits_make_nothing() {
        let printed = "[main abc1234] add the thing\n";
        assert!(shas("git status", printed).is_empty());
        assert!(shas("git log --oneline -3", printed).is_empty());
        assert!(shas("git commit-tree HEAD^{tree}", printed).is_empty());
        assert!(shas("echo \"git commit -m x\"", printed).is_empty());
        assert!(shas("git -C commit status", printed).is_empty());
        assert!(shas("git commit -m x", "nothing to commit, working tree clean\n").is_empty());
        assert!(shas("git commit -m x", "On branch main\n[not a sha]\n[main xyz]\n").is_empty());
    }

    #[test]
    fn a_commit_that_printed_nothing_has_no_sha() {
        let at = |d: &str| Some(d.to_string());
        assert_eq!(shas("git commit -qm x", ""), [(None, at("/w/p"))]);
        assert_eq!(shas("git -C a commit -q -m x && git -C b commit -q -m y", " \n"), [(None, at("/w/p/a")), (None, at("/w/p/b"))]);
        assert!(shas("git status", "").is_empty());
    }
}
