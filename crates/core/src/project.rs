use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub id: String,
    pub root: PathBuf,
}

const MARKERS: &[&str] = &[
    "Cargo.toml", "package.json", "pyproject.toml", "go.mod", "deno.json", "build.gradle", "build.gradle.kts",
    "pom.xml", "Gemfile", "composer.json", "project.godot", "CMakeLists.txt", "Makefile", ".magicbox",
];
const CODE_DIRS: &[&str] = &["Projects", "projects", "code", "src", "dev", "repos", "magicbox", "active"];

pub fn project_of(path: &Path) -> Option<Project> {
    let start = if path.is_file() { path.parent()? } else { path };
    let Some(root) = start.ancestors().find(|a| a.join(".git").exists()) else {
        return folder_project(start);
    };
    if dirs::home_dir().as_deref() == Some(root) {
        return None;
    }
    let id = git_config(root)
        .and_then(|c| fs::read_to_string(c).ok())
        .and_then(|c| origin_url(&c))
        .and_then(|u| normalize_remote(&u))
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))?;
    Some(Project { id, root: root.to_path_buf() })
}

pub fn git_config(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if dot.is_dir() {
        return Some(dot.join("config"));
    }
    let text = fs::read_to_string(&dot).ok()?;
    let gitdir = root.join(text.trim().strip_prefix("gitdir:")?.trim());
    match fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => Some(gitdir.join(c.trim()).join("config")),
        Err(_) => Some(gitdir.join("config")),
    }
}

// No git: the topmost folder under home with a project marker file, else the direct child of a code folder like ~/Projects.
fn folder_project(start: &Path) -> Option<Project> {
    let home = dirs::home_dir()?;
    let rel = start.strip_prefix(&home).ok()?;
    if rel.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.')) {
        return None;
    }
    let mut cur = home.clone();
    let mut fallback = None;
    let mut parent_is_code_dir = false;
    for comp in rel.components() {
        cur.push(comp);
        let name = comp.as_os_str().to_string_lossy();
        if MARKERS.iter().any(|m| cur.join(m).exists()) {
            return Some(Project { id: name.into_owned(), root: cur });
        }
        if parent_is_code_dir && fallback.is_none() {
            fallback = Some(Project { id: name.to_string(), root: cur.clone() });
        }
        parent_is_code_dir = CODE_DIRS.contains(&name.as_ref());
    }
    fallback
}

pub fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let name: Vec<&str> = line.trim_matches(|c| c == '[' || c == ']').split_whitespace().collect();
            in_origin = name == ["remote", "\"origin\""];
        } else if in_origin {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == "url" {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map(|(_, p)| p)?,
        None => match url.split_once(':') {
            Some((_, p)) => p,
            None => url,
        },
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && *p != "..").collect();
    match parts.as_slice() {
        [] => None,
        [one] => Some(one.to_string()),
        [.., owner, repo] => Some(format!("{owner}/{repo}")),
    }
}
