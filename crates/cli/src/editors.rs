use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use kairos_core::paths;
use serde::{Deserialize, Serialize};

/// Hackatime's plugin for VS Code and the editors built on it, the one Hackatime's installer adds.
const EXTENSION: &str = "hackatime.hackatime-time-tracker";
const EXTENSIONS_WITH_HEARTBEATS: [&str; 2] = ["wakatime.vscode-wakatime", EXTENSION];
const JETBRAINS_PLUGIN: &str = "com.wakatime.intellij.plugin";
const ADDED: &str = "editor-plugins.json";
/// How long a JetBrains launcher that hands off and returns early gets to finish putting the plugin in.
const JETBRAINS_WAIT: Duration = Duration::from_secs(30);

struct Code {
    name: &'static str,
    cli: &'static str,
    home_dir: &'static str,
    mac_app: &'static str,
    win_folder: &'static str,
}

const CODE: [Code; 6] = [
    Code { name: "VS Code", cli: "code", home_dir: ".vscode", mac_app: "Visual Studio Code", win_folder: "Microsoft VS Code" },
    Code { name: "Cursor", cli: "cursor", home_dir: ".cursor", mac_app: "Cursor", win_folder: "cursor" },
    Code { name: "Windsurf", cli: "windsurf", home_dir: ".windsurf", mac_app: "Windsurf", win_folder: "Windsurf" },
    Code { name: "Antigravity", cli: "antigravity", home_dir: ".antigravity", mac_app: "Antigravity", win_folder: "Antigravity" },
    Code { name: "VSCodium", cli: "codium", home_dir: ".vscode-oss", mac_app: "VSCodium", win_folder: "VSCodium" },
    Code { name: "Trae", cli: "trae", home_dir: ".trae", mac_app: "Trae", win_folder: "Trae" },
];

struct JetBrains {
    name: &'static str,
    /// The start of the IDE's config folder names, `IntelliJIdea2025.1` and the like.
    codes: &'static [&'static str],
    cli: &'static str,
}

const JETBRAINS: [JetBrains; 10] = [
    JetBrains { name: "IntelliJ IDEA", codes: &["IntelliJIdea", "IdeaIC"], cli: "idea" },
    JetBrains { name: "PyCharm", codes: &["PyCharm", "PyCharmCE"], cli: "pycharm" },
    JetBrains { name: "WebStorm", codes: &["WebStorm"], cli: "webstorm" },
    JetBrains { name: "GoLand", codes: &["GoLand"], cli: "goland" },
    JetBrains { name: "RustRover", codes: &["RustRover"], cli: "rustrover" },
    JetBrains { name: "RubyMine", codes: &["RubyMine"], cli: "rubymine" },
    JetBrains { name: "PhpStorm", codes: &["PhpStorm"], cli: "phpstorm" },
    JetBrains { name: "CLion", codes: &["CLion"], cli: "clion" },
    JetBrains { name: "DataGrip", codes: &["DataGrip"], cli: "datagrip" },
    JetBrains { name: "Rider", codes: &["Rider"], cli: "rider" },
];

/// A plugin Kairos added, so uninstall can take out exactly that.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Added {
    Extension { editor: String, cli: PathBuf, id: String },
    Folder { editor: String, path: PathBuf },
}

/// An editor found without a plugin that sends heartbeats.
struct Missing {
    name: &'static str,
    cli: PathBuf,
    jetbrains: Option<&'static JetBrains>,
}

/// Adds Hackatime's plugin to the editors that have no WakaTime or Hackatime plugin yet. Hack Club's hours come from
/// Hackatime, so this isn't a question, and each one is said and recorded so uninstall takes out exactly these.
pub fn add_missing() {
    let Some(home) = dirs::home_dir() else { return };
    let missing = missing(&home);
    let data = paths::data_dir();
    for m in missing {
        let added = match m.jetbrains {
            None => add_extension(m.name, &m.cli),
            Some(ide) => add_jetbrains(ide, &m.cli, &home),
        };
        match added.and_then(|a| record(&data, a)) {
            Ok(()) => println!("Added Hackatime's plugin to {}. It starts the next time you open it.", m.name),
            Err(e) => println!("Could not add Hackatime's plugin to {}: {e:#}", m.name),
        }
    }
}

/// Takes out the plugins Kairos added and nothing else.
pub fn remove_added() {
    let data = paths::data_dir();
    for a in load(&data) {
        let editor = match &a {
            Added::Extension { editor, .. } | Added::Folder { editor, .. } => editor,
        };
        if take_out(&a) {
            println!("Took Hackatime's plugin out of {editor}.");
        } else {
            println!("Could not take Hackatime's plugin out of {editor}. Remove it in the editor's plugin settings if you don't want it.");
        }
    }
    let _ = fs::remove_file(data.join(ADDED));
}

fn take_out(added: &Added) -> bool {
    match added {
        Added::Extension { cli, id, .. } => !cli.exists() || run(cli, &["--uninstall-extension", id]),
        Added::Folder { path, .. } => !path.exists() || fs::remove_dir_all(path).is_ok(),
    }
}

fn missing(home: &Path) -> Vec<Missing> {
    let mut out = Vec::new();
    for c in &CODE {
        let Some(cli) = find(c.cli, &code_fallbacks(c, home)) else { continue };
        if !code_has_heartbeats(&cli, &home.join(c.home_dir).join("extensions")) {
            out.push(Missing { name: c.name, cli, jetbrains: None });
        }
    }
    for ide in &JETBRAINS {
        let Some(cli) = jetbrains_cli(ide, home) else { continue };
        if jetbrains_dirs(ide, home).last().is_some_and(|newest| wakatime_folders(newest).is_empty()) {
            out.push(Missing { name: ide.name, cli, jetbrains: Some(ide) });
        }
    }
    out
}

fn add_extension(editor: &str, cli: &Path) -> Result<Added> {
    if !run(cli, &["--install-extension", EXTENSION]) {
        anyhow::bail!("{} --install-extension {EXTENSION} did not finish", cli.display());
    }
    Ok(Added::Extension { editor: editor.into(), cli: cli.into(), id: EXTENSION.into() })
}

fn add_jetbrains(ide: &JetBrains, cli: &Path, home: &Path) -> Result<Added> {
    let before: Vec<PathBuf> = jetbrains_dirs(ide, home).iter().flat_map(|d| wakatime_folders(d)).collect();
    let ran = run(cli, &["installPlugins", JETBRAINS_PLUGIN]);
    let deadline = Instant::now() + if ran { JETBRAINS_WAIT } else { Duration::ZERO };
    let mut waiting = false;
    let new = loop {
        let new = jetbrains_dirs(ide, home).iter().flat_map(|d| wakatime_folders(d)).find(|p| !before.contains(p));
        if new.is_some() || Instant::now() >= deadline {
            break new;
        }
        if !waiting {
            println!("Waiting for {} to finish adding it...", ide.name);
            waiting = true;
        }
        thread::sleep(Duration::from_secs(1));
    };
    match new {
        Some(path) => Ok(Added::Folder { editor: ide.name.into(), path }),
        None if ran => anyhow::bail!("close {} and run kairos install again", ide.name),
        None => anyhow::bail!("{} installPlugins {JETBRAINS_PLUGIN} did not finish (close {} first)", cli.display(), ide.name),
    }
}

fn sends_heartbeats(id: &str) -> bool {
    EXTENSIONS_WITH_HEARTBEATS.iter().any(|w| id.trim().eq_ignore_ascii_case(w))
}

/// The editor's own list, which also sees a portable install or another extensions folder, and the usual folder when
/// the list can't be had.
fn code_has_heartbeats(cli: &Path, extensions: &Path) -> bool {
    match output(cli, &["--list-extensions"]) {
        Some(list) => list.lines().any(sends_heartbeats),
        None => code_has_plugin(extensions),
    }
}

/// VS Code keeps the installed extensions in extensions.json; older versions only have their folders.
fn code_has_plugin(extensions: &Path) -> bool {
    if let Ok(text) = fs::read_to_string(extensions.join("extensions.json")) {
        let list: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
        return list.iter().filter_map(|e| e.pointer("/identifier/id")?.as_str()).any(sends_heartbeats);
    }
    let Ok(dir) = fs::read_dir(extensions) else { return false };
    dir.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_lowercase();
        let version = |w: &str| name.strip_prefix(w)?.strip_prefix('-').map(|v| v.starts_with(|c: char| c.is_ascii_digit()));
        EXTENSIONS_WITH_HEARTBEATS.iter().any(|w| version(w) == Some(true))
    })
}

/// Where JetBrains IDEs keep each version's settings, and the plugins folder for a version's folder name.
fn jetbrains_layout(home: &Path) -> (PathBuf, impl Fn(&str) -> PathBuf) {
    let (config, plugins) = if cfg!(windows) {
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Roaming"));
        (appdata.join("JetBrains"), None)
    } else if cfg!(target_os = "macos") {
        (home.join("Library/Application Support/JetBrains"), None)
    } else {
        (home.join(".config/JetBrains"), Some(home.join(".local/share/JetBrains")))
    };
    let base = config.clone();
    (config, move |name: &str| match &plugins {
        Some(dir) => dir.join(name),
        None => base.join(name).join("plugins"),
    })
}

/// The plugins folder of each installed version of the IDE, oldest first.
fn jetbrains_dirs(ide: &JetBrains, home: &Path) -> Vec<PathBuf> {
    let (config, plugins) = jetbrains_layout(home);
    let Ok(dir) = fs::read_dir(&config) else { return Vec::new() };
    let mut found: Vec<(String, String)> = dir
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| {
            let version = ide.codes.iter().find_map(|c| name.strip_prefix(c).filter(|v| v.starts_with(|ch: char| ch.is_ascii_digit())))?;
            Some((version.to_string(), name))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, name)| plugins(&name)).collect()
}

fn wakatime_folders(plugins: &Path) -> Vec<PathBuf> {
    let Ok(dir) = fs::read_dir(plugins) else { return Vec::new() };
    let wakatime = |p: &Path| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("wakatime"));
    dir.flatten().map(|e| e.path()).filter(|p| p.is_dir() && wakatime(p)).collect()
}

fn code_fallbacks(c: &Code, home: &Path) -> Vec<PathBuf> {
    if cfg!(windows) {
        let file = format!("{}.cmd", c.cli);
        [std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs")), std::env::var_os("ProgramFiles").map(PathBuf::from)]
            .into_iter()
            .flatten()
            .flat_map(|d| [d.join(c.win_folder).join("bin").join(&file), d.join(c.win_folder).join("resources/app/bin").join(&file)])
            .collect()
    } else if cfg!(target_os = "macos") {
        let app = format!("Applications/{}.app/Contents/Resources/app/bin/{}", c.mac_app, c.cli);
        vec![Path::new("/").join(&app), home.join(&app)]
    } else {
        vec![PathBuf::from("/snap/bin").join(c.cli), home.join(".local/bin").join(c.cli)]
    }
}

/// The IDE's own launcher comes before the PATH and Toolbox's scripts, which can hand off to the app and return before
/// the plugin is in.
fn jetbrains_cli(ide: &JetBrains, home: &Path) -> Option<PathBuf> {
    let (launchers, scripts) = jetbrains_fallbacks(ide, home);
    launchers
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| on_path(ide.cli, std::env::var_os("PATH")?))
        .or_else(|| scripts.into_iter().find(|p| p.is_file()))
}

/// Launchers inside the folders IDEs are installed in (newest name first), and Toolbox's script.
fn jetbrains_fallbacks(ide: &JetBrains, home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (parents, inside, script) = if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Local"));
        let mut parents = vec![local.join("Programs")];
        parents.extend(std::env::var_os("ProgramFiles").map(|d| PathBuf::from(d).join("JetBrains")));
        (parents, vec![format!("bin/{}64.exe", ide.cli)], local.join("JetBrains/Toolbox/scripts").join(format!("{}.cmd", ide.cli)))
    } else if cfg!(target_os = "macos") {
        let script = home.join("Library/Application Support/JetBrains/Toolbox/scripts").join(ide.cli);
        (vec![PathBuf::from("/Applications"), home.join("Applications")], vec![format!("Contents/MacOS/{}", ide.cli)], script)
    } else {
        let toolbox = home.join(".local/share/JetBrains/Toolbox");
        let inside = vec![format!("bin/{}", ide.cli), format!("bin/{}.sh", ide.cli)];
        (vec![toolbox.join("apps"), PathBuf::from("/opt")], inside, toolbox.join("scripts").join(ide.cli))
    };
    let mut folders: Vec<PathBuf> = parents.iter().filter_map(|p| fs::read_dir(p).ok()).flatten().flatten().map(|e| e.path()).collect();
    folders.sort_by(|a, b| b.cmp(a));
    let launchers = folders.iter().flat_map(|f| inside.iter().map(move |i| f.join(i))).collect();
    (launchers, vec![script])
}

fn find(name: &str, fallbacks: &[PathBuf]) -> Option<PathBuf> {
    on_path(name, std::env::var_os("PATH")?).or_else(|| fallbacks.iter().find(|p| p.is_file()).cloned())
}

fn on_path(name: &str, path: OsString) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) { [".cmd", ".exe", ".bat"].iter().map(|x| format!("{name}{x}")).collect() } else { vec![name.into()] };
    std::env::split_paths(&path).flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

fn command(cli: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(cli);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn run(cli: &Path, args: &[&str]) -> bool {
    command(cli, args).status().is_ok_and(|s| s.success())
}

fn output(cli: &Path, args: &[&str]) -> Option<String> {
    let out = command(cli, args).stdout(Stdio::piped()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn load(data: &Path) -> Vec<Added> {
    fs::read_to_string(data.join(ADDED)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn record(data: &Path, added: Added) -> Result<()> {
    let mut all = load(data);
    if !all.contains(&added) {
        all.push(added);
    }
    fs::create_dir_all(data).with_context(|| format!("creating {}", data.display()))?;
    let target = data.join(ADDED);
    let tmp = target.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&all)?).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &target).with_context(|| format!("writing {}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Temp {
            let dir = std::env::temp_dir().join(format!("kairos-editors-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn vs_code_plugins_are_read_from_extensions_json_or_the_folders() {
        let t = Temp::new("code");
        let ext = t.0.join("extensions");
        assert!(!code_has_plugin(&ext), "no extensions folder");
        fs::create_dir_all(ext.join("wakatime.vscode-wakatime-25.0.0")).unwrap();
        assert!(code_has_plugin(&ext));
        fs::write(ext.join("extensions.json"), r#"[{"identifier":{"id":"rust-lang.rust-analyzer"}}]"#).unwrap();
        assert!(!code_has_plugin(&ext), "extensions.json wins over a folder left behind");
        fs::write(ext.join("extensions.json"), r#"[{"identifier":{"id":"Hackatime.hackatime-time-tracker"}}]"#).unwrap();
        assert!(code_has_plugin(&ext));
        fs::remove_file(ext.join("extensions.json")).unwrap();
        fs::remove_dir(ext.join("wakatime.vscode-wakatime-25.0.0")).unwrap();
        fs::create_dir_all(ext.join("wakatime.vscode-wakatime-themes-1.0.0")).unwrap();
        assert!(!code_has_plugin(&ext), "another extension that starts the same way");
    }

    #[cfg(unix)]
    #[test]
    fn the_editors_own_list_comes_before_its_folder() {
        use std::os::unix::fs::PermissionsExt;
        let t = Temp::new("list");
        let ext = t.0.join("extensions");
        fs::create_dir_all(ext.join("wakatime.vscode-wakatime-25.0.0")).unwrap();
        let cli = t.0.join("code");
        let script = |body: &str| {
            fs::write(&cli, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        };
        script("echo rust-lang.rust-analyzer");
        assert!(!code_has_heartbeats(&cli, &ext), "the list wins over a folder left behind");
        script("printf 'rust-lang.rust-analyzer\\nWakaTime.vscode-wakatime\\n'");
        assert!(code_has_heartbeats(&cli, &ext));
        script("exit 1");
        assert!(code_has_heartbeats(&cli, &ext), "the folder when the list fails");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_jetbrains_ides_own_launcher_comes_before_toolboxs_script() {
        let t = Temp::new("launcher");
        let home = &t.0;
        let idea = &JETBRAINS[0];
        let toolbox = home.join(".local/share/JetBrains/Toolbox");
        fs::create_dir_all(toolbox.join("scripts")).unwrap();
        fs::write(toolbox.join("scripts/idea"), "").unwrap();
        let (launchers, scripts) = jetbrains_fallbacks(idea, home);
        assert_eq!(launchers.iter().find(|p| p.is_file()), None);
        assert_eq!(scripts, vec![toolbox.join("scripts/idea")]);
        for app in ["intellij-idea-community-edition", "intellij-idea-ultimate", "pycharm-community"] {
            fs::create_dir_all(toolbox.join("apps").join(app).join("bin")).unwrap();
        }
        fs::write(toolbox.join("apps/intellij-idea-community-edition/bin/idea.sh"), "").unwrap();
        fs::write(toolbox.join("apps/intellij-idea-ultimate/bin/idea"), "").unwrap();
        fs::write(toolbox.join("apps/pycharm-community/bin/pycharm.sh"), "").unwrap();
        let (launchers, _) = jetbrains_fallbacks(idea, home);
        assert_eq!(launchers.into_iter().find(|p| p.is_file()), Some(toolbox.join("apps/intellij-idea-ultimate/bin/idea")));
    }

    #[cfg(not(windows))]
    #[test]
    fn jetbrains_versions_and_their_wakatime_folders() {
        let t = Temp::new("jb");
        let home = &t.0;
        let idea = &JETBRAINS[0];
        assert!(jetbrains_dirs(idea, home).is_empty());
        let (config, plugins) = jetbrains_layout(home);
        for name in ["IntelliJIdea2023.2", "IdeaIC2024.3", "IntelliJIdeaX", "PyCharm2025.1"] {
            fs::create_dir_all(config.join(name)).unwrap();
        }
        let dirs = jetbrains_dirs(idea, home);
        assert_eq!(dirs, vec![plugins("IntelliJIdea2023.2"), plugins("IdeaIC2024.3")], "by version, other IDEs left out");
        let linux = home.join(".local/share/JetBrains/IdeaIC2024.3");
        let mac = home.join("Library/Application Support/JetBrains/IdeaIC2024.3/plugins");
        assert_eq!(dirs[1], if cfg!(target_os = "macos") { mac } else { linux });
        fs::create_dir_all(dirs[1].join("WakaTime")).unwrap();
        fs::create_dir_all(dirs[1].join("rust")).unwrap();
        assert_eq!(wakatime_folders(&dirs[1]), vec![dirs[1].join("WakaTime")]);
        assert!(wakatime_folders(&dirs[0]).is_empty());
    }

    #[test]
    fn programs_are_found_on_the_path_before_the_usual_places() {
        let t = Temp::new("path");
        let (a, b) = (t.0.join("a"), t.0.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let file = if cfg!(windows) { "code.cmd" } else { "code" };
        fs::write(b.join(file), "").unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        assert_eq!(on_path("code", path.clone()), Some(b.join(file)));
        assert_eq!(on_path("cursor", path), None);
    }

    #[cfg(unix)]
    #[test]
    fn added_plugins_are_recorded_once_and_taken_out_again() {
        use std::os::unix::fs::PermissionsExt;
        let t = Temp::new("added");
        let data = t.0.join("data");
        let log = t.0.join("log");
        let cli = t.0.join("code");
        fs::write(&cli, format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display())).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();

        let a = add_extension("VS Code", &cli).unwrap();
        record(&data, a.clone()).unwrap();
        record(&data, a.clone()).unwrap();
        let folder = t.0.join("plugins").join("WakaTime");
        fs::create_dir_all(&folder).unwrap();
        record(&data, Added::Folder { editor: "PyCharm".into(), path: folder.clone() }).unwrap();
        assert_eq!(load(&data), vec![a, Added::Folder { editor: "PyCharm".into(), path: folder.clone() }]);

        assert!(load(&data).iter().all(take_out));
        assert_eq!(fs::read_to_string(&log).unwrap(), format!("--install-extension {EXTENSION}\n--uninstall-extension {EXTENSION}\n"));
        assert!(!folder.exists());
    }
}
