use std::fs;
use std::path::PathBuf;

use crate::classify::{normalize, Procs, Tree};

pub struct ProcFs {
    tree: Tree,
}

impl ProcFs {
    pub fn scan() -> ProcFs {
        let mut tree = Tree::default();
        for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
            let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else { continue };
            let Some(pp) = rest.split_whitespace().nth(1).and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            tree.insert(pid, pp);
        }
        ProcFs { tree }
    }
}

impl Procs for ProcFs {
    const TITLE_HINTS: bool = false;

    fn tree(&self) -> &Tree {
        &self.tree
    }

    fn name(&self, pid: u32) -> String {
        self.exe(pid)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .map(|n| normalize(n.trim_end_matches(" (deleted)")))
            .unwrap_or_default()
    }

    fn comm(&self, pid: u32) -> String {
        fs::read_to_string(format!("/proc/{pid}/comm")).map(|s| normalize(s.trim())).unwrap_or_default()
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    fn cwd(&self, pid: u32) -> Option<PathBuf> {
        fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    fn cmdline(&self, pid: u32) -> Vec<String> {
        fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| {
                b.split(|&c| c == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

