
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf, Prefix};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::account::write_private;

const FILE: &str = "exclusions.json";
pub const MAX_ROOTS: usize = 256;
const FOLD_CASE: bool = cfg!(any(windows, target_os = "macos"));

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Exclusions {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

fn parts(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut base = 0;
    for c in path.components() {
        let part = match c {
            Component::CurDir => continue,
            Component::ParentDir => {
                if out.len() > base {
                    out.pop();
                }
                continue;
            }
            Component::Prefix(p) => {
                base += 1;
                match p.kind() {
                    Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:", d as char),
                    _ => p.as_os_str().to_string_lossy().into_owned(),
                }
            }
            Component::RootDir => {
                base += 1;
                "/".into()
            }
            Component::Normal(n) => n.to_string_lossy().into_owned(),
        };
        out.push(if FOLD_CASE { part.to_lowercase() } else { part });
    }
    out
}

impl Exclusions {
    /// The list holds folders with links resolved, while editors report the path they were given,
    /// so a path the list does not cover as written is checked again with its links resolved.
    pub fn covers(&self, path: &Path) -> bool {
        if self.roots.is_empty() {
            return false;
        }
        self.covers_as_written(path) || resolved(path).is_some_and(|p| self.covers_as_written(&p))
    }

    fn covers_as_written(&self, path: &Path) -> bool {
        let path = parts(path);
        self.roots.iter().filter(|r| r.is_absolute()).any(|r| path.starts_with(&parts(r)))
    }
}

// A file that is gone, or not written yet, resolves through its folder.
fn resolved(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    fs::canonicalize(path).ok().or_else(|| Some(fs::canonicalize(path.parent()?).ok()?.join(path.file_name()?)))
}

fn plain(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        if let Some(rest) = path.to_str().and_then(|s| s.strip_prefix(r"\\?\")).filter(|r| r.as_bytes().get(1) == Some(&b':')) {
            return PathBuf::from(rest);
        }
    }
    path
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().context("cannot find the current folder")?.join(path)
    };
    Ok(fs::canonicalize(&full).map(plain).unwrap_or_else(|_| lexical(&full)))
}

pub fn load(data: &Path) -> Result<Exclusions> {
    let path = data.join(FILE);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Exclusions::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn save(data: &Path, ex: &Exclusions) -> Result<()> {
    fs::create_dir_all(data).with_context(|| format!("creating {}", data.display()))?;
    let target = data.join(FILE);
    let tmp = target.with_extension("json.tmp");
    write_private(&tmp, serde_json::to_string_pretty(ex)?.as_bytes()).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &target).with_context(|| format!("writing {}", target.display()))
}

// Excluding the home folder or a drive would turn Kairos off without saying so.
fn check(root: &Path, home: Option<&Path>) -> Result<()> {
    let everything = Exclusions { roots: vec![root.to_path_buf()] };
    if root.parent().is_none() || home.is_some_and(|h| everything.covers(h)) {
        bail!("{} holds your whole home folder, so Kairos would record nothing. Use kairos pause to stop for a while, or pick a folder inside it.", root.display());
    }
    Ok(())
}

pub fn add(data: &Path, path: &Path) -> Result<PathBuf> {
    let root = absolute(path)?;
    check(&root, dirs::home_dir().and_then(|h| absolute(&h).ok()).as_deref())?;
    let mut ex = load(data)?;
    let wanted = parts(&root);
    if let Some(known) = ex.roots.iter().find(|r| parts(r) == wanted) {
        return Ok(known.clone());
    }
    if ex.roots.len() >= MAX_ROOTS {
        bail!("{MAX_ROOTS} folders are excluded already. Remove some with kairos exclude --remove first.");
    }
    ex.roots.push(root.clone());
    save(data, &ex)?;
    Ok(root)
}

pub fn remove(data: &Path, path: &Path) -> Result<bool> {
    let mut ex = load(data)?;
    let wanted = parts(&absolute(path)?);
    let before = ex.roots.len();
    ex.roots.retain(|r| parts(r) != wanted);
    if ex.roots.len() == before {
        return Ok(false);
    }
    save(data, &ex)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(path: &str) -> PathBuf {
        PathBuf::from(format!("{}{path}", if cfg!(windows) { "C:" } else { "" }))
    }

    fn excluding(roots: &[&str]) -> Exclusions {
        Exclusions { roots: roots.iter().map(|r| abs(r)).collect() }
    }

    fn data(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kairos-exclude-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_folder_covers_what_is_inside_it_by_whole_components() {
        let ex = excluding(&["/a/b"]);
        for inside in ["/a/b", "/a/b/c", "/a/b/c/d.rs", "/a/b/", "/a/b/./c", "/a/x/../b/c", "/a/b/c/.."] {
            assert!(ex.covers(&abs(inside)), "{inside}");
        }
        for outside in ["/a", "/a/bc", "/a/c/b", "/a/b/..", "/b", "/", "/a/b/../c", "a/b/c"] {
            assert!(!ex.covers(&abs(outside)), "{outside}");
        }
        assert!(!Exclusions::default().covers(&abs("/a/b")));
        assert!(!excluding(&["/a/b", "/c"]).covers(&abs("/d")));
        assert!(excluding(&["/a/b", "/c"]).covers(&abs("/c/d")));
    }

    #[test]
    fn a_trailing_separator_in_the_list_changes_nothing() {
        let ex = Exclusions { roots: vec![PathBuf::from(format!("{}/", abs("/a/b").display()))] };
        assert!(ex.covers(&abs("/a/b/c")));
        assert!(!ex.covers(&abs("/a/bc")));
    }

    #[test]
    fn case_matters_only_where_the_file_system_ignores_it() {
        let ex = excluding(&["/Work/Secret"]);
        assert_eq!(ex.covers(&abs("/work/secret/a.rs")), FOLD_CASE);
        assert!(ex.covers(&abs("/Work/Secret/a.rs")));
        #[cfg(target_os = "linux")]
        assert!(!ex.covers(&abs("/WORK/SECRET")));
        #[cfg(any(windows, target_os = "macos"))]
        assert!(ex.covers(&abs("/WORK/SECRET")));
    }

    #[test]
    fn roots_that_are_not_absolute_cover_nothing() {
        let ex = Exclusions { roots: vec![PathBuf::new(), PathBuf::from("a")] };
        assert!(!ex.covers(&abs("/a")));
        assert!(!ex.covers(Path::new("a/b")));
    }

    #[test]
    fn relative_paths_are_made_absolute() {
        let here = std::env::current_dir().unwrap();
        let made = absolute(Path::new("no-such-folder-yet/../secret")).unwrap();
        assert!(made.is_absolute());
        assert!(made.ends_with("secret") && !made.to_string_lossy().contains(".."));
        assert_eq!(parts(&made), parts(&here.join("secret")));
    }

    #[test]
    fn the_home_folder_and_drives_are_refused() {
        let home = abs("/home/ada");
        let home = Some(home.as_path());
        assert!(check(&abs("/home/ada/secret"), home).is_ok());
        assert!(check(&abs("/home/ada/secret"), None).is_ok());
        for root in ["/home/ada", "/home", "/home/ada/."] {
            assert!(check(&abs(root), home).is_err(), "{root}");
        }
        assert!(check(&abs("/"), None).is_err());
        assert!(check(&abs("/home/adam"), home).is_ok());
    }

    #[test]
    fn add_list_and_remove_survive_a_reload() {
        let dir = data("roundtrip");
        assert_eq!(load(&dir).unwrap(), Exclusions::default());
        let secret = dir.join("work").join("secret");
        let stored = add(&dir, &secret).unwrap();
        assert_eq!(stored, absolute(&secret).unwrap());
        assert_eq!(add(&dir, &secret.join("..").join("secret")).unwrap(), stored, "the same folder twice is one entry");
        let other = add(&dir, &dir.join("work").join("secrets")).unwrap();
        let ex = load(&dir).unwrap();
        assert_eq!(ex.roots, [stored.clone(), other]);
        assert!(ex.covers(&stored.join("a.rs")));
        assert!(!ex.covers(&dir.join("work").join("secretive")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dir.join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(remove(&dir, &secret).unwrap());
        assert!(!remove(&dir, &secret).unwrap());
        assert_eq!(load(&dir).unwrap().roots.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_is_covered_through_links_either_way() {
        let dir = data("links");
        fs::create_dir_all(dir.join("disk").join("code").join("secret")).unwrap();
        std::os::unix::fs::symlink(dir.join("disk").join("code"), dir.join("code")).unwrap();
        let (linked, real) = (dir.join("code").join("secret"), dir.join("disk").join("code").join("secret"));

        add(&dir, &linked).unwrap();
        let ex = load(&dir).unwrap();
        assert!(ex.covers(&real.join("a.rs")), "the folder the link points at");
        assert!(ex.covers(&linked.join("a.rs")), "a file not written yet, through the link");
        fs::write(real.join("b.rs"), "").unwrap();
        assert!(ex.covers(&linked.join("b.rs")));
        assert!(!ex.covers(&dir.join("code").join("open").join("a.rs")));

        remove(&dir, &linked).unwrap();
        add(&dir, &real).unwrap();
        assert!(load(&dir).unwrap().covers(&linked.join("b.rs")), "excluded by its real path, reached through the link");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_list_is_capped_and_a_broken_file_is_left_alone() {
        let dir = data("cap");
        fs::create_dir_all(&dir).unwrap();
        let full = Exclusions { roots: (0..MAX_ROOTS).map(|i| dir.join(format!("f{i}"))).collect() };
        save(&dir, &full).unwrap();
        assert!(add(&dir, &dir.join("one-more")).is_err());
        assert!(add(&dir, &dir.join("f3")).is_ok(), "a folder already on the list is not a new one");
        fs::write(dir.join(FILE), "not json").unwrap();
        assert!(load(&dir).is_err());
        assert!(add(&dir, &dir.join("x")).is_err());
        assert_eq!(fs::read_to_string(dir.join(FILE)).unwrap(), "not json");
        let _ = fs::remove_dir_all(&dir);
    }
}
