
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer};
pub use ed25519_dalek::{SigningKey, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{account, paths};

/// Release signing keys, newest first. Add the next key here one release before signing with it.
pub const TRUSTED_KEYS: &[&str] = &["L0WyV4HaTE6v2Ey0dGMbi2J/yBB0vH2HE4/tb5CZhhg="];

pub const TARGETS: &[&str] = &["windows-x86_64", "macos-aarch64", "macos-x86_64", "linux-x86_64", "linux-aarch64"];

pub const MANIFEST: &str = "manifest.json";
pub const SIGNATURE: &str = "manifest.json.sig";
pub const SUMS: &str = "SHA256SUMS";

#[cfg(feature = "client")]
const MAX_MANIFEST: u64 = 256 * 1024;
#[cfg(feature = "client")]
const MAX_SIGNATURE: u64 = 1024;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub published_at: String,
    pub targets: BTreeMap<String, Target>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub files: BTreeMap<String, FileInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub sha256: String,
    pub size: u64,
}

impl Manifest {
    /// The exact bytes that get signed: pretty JSON (keys in a fixed order) and a final newline.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec_pretty(self).expect("manifest serializes");
        out.push(b'\n');
        out
    }
}

pub fn target_for_triple(triple: &str) -> Option<&'static str> {
    let arch = triple.split('-').next()?;
    let os = if triple.contains("-windows") {
        "windows"
    } else if triple.contains("-apple-darwin") {
        "macos"
    } else if triple.contains("-linux") {
        "linux"
    } else {
        return None;
    };
    target_name(os, arch)
}

pub fn target_name(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("windows", "x86_64") => "windows-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        _ => return None,
    })
}

pub fn current_target() -> Option<&'static str> {
    target_name(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn binaries(target: &str) -> [String; 2] {
    let ext = if target.starts_with("windows-") { ".exe" } else { "" };
    [format!("kairos{ext}"), format!("kairosd{ext}")]
}

pub fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// A release version: semver without build metadata (it would not survive a URL path).
pub fn release_version(s: &str) -> Result<Version> {
    let v = Version::parse(s).with_context(|| format!("{s:?} is not a valid version"))?;
    ensure!(v.build.is_empty(), "release version {s:?} must not have build metadata");
    Ok(v)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sums_file(files: &BTreeMap<String, FileInfo>) -> String {
    files.iter().map(|(name, f)| format!("{}  {name}\n", f.sha256)).collect()
}

fn check(m: &Manifest) -> Result<Version> {
    let version = release_version(&m.version)?;
    for (target, t) in &m.targets {
        ensure!(plain_name(target), "the manifest has a bad target name {target:?}");
        for (name, f) in &t.files {
            ensure!(plain_name(name), "the manifest has a bad file name {name:?}");
            ensure!(
                f.sha256.len() == 64 && f.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                "the manifest has a bad checksum for {target}/{name}"
            );
            ensure!(f.size <= MAX_FILE, "{target}/{name} is too large");
        }
    }
    Ok(version)
}

pub fn parse_key(b64: &str) -> Result<VerifyingKey> {
    let bytes = B64.decode(b64.trim()).context("public key is not base64")?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| anyhow::anyhow!("public key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&bytes).context("not a valid ed25519 public key")
}

pub fn encode_key(key: &VerifyingKey) -> String {
    B64.encode(key.as_bytes())
}

pub fn trusted_keys() -> Vec<VerifyingKey> {
    TRUSTED_KEYS.iter().map(|k| parse_key(k).expect("compiled-in release key is valid")).collect()
}

pub fn signing_key(seed_b64: &str) -> Result<SigningKey> {
    let bytes = B64.decode(seed_b64.trim()).context("signing key is not base64")?;
    let seed: [u8; 32] = bytes.try_into().map_err(|_| anyhow::anyhow!("signing key must be a 32-byte seed"))?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn sign(manifest: &[u8], key: &SigningKey) -> String {
    format!("{}\n", B64.encode(key.sign(manifest).to_bytes()))
}

#[derive(Debug, Clone)]
pub struct Release {
    pub manifest: Manifest,
    pub version: Version,
}

impl Release {
    pub fn verify(manifest: &[u8], sig: &[u8], keys: &[VerifyingKey]) -> Result<Release> {
        let text = std::str::from_utf8(sig).context("the release signature is not text")?;
        let raw = B64.decode(text.trim()).context("the release signature is not base64")?;
        let sig = Signature::from_slice(&raw).context("the release signature has the wrong length")?;
        ensure!(
            keys.iter().any(|k| k.verify_strict(manifest, &sig).is_ok()),
            "the release manifest is not signed by a trusted Kairos key"
        );
        let manifest: Manifest = serde_json::from_slice(manifest).context("the release manifest is not valid")?;
        let version = check(&manifest)?;
        Ok(Release { manifest, version })
    }

    pub fn newer_than(&self, current: &str) -> Result<Option<&Version>> {
        let current = Version::parse(current).with_context(|| format!("{current:?} is not a valid version"))?;
        Ok((self.version > current).then_some(&self.version))
    }
}

pub fn server_for(data_dir: &Path) -> String {
    account::load(data_dir)
        .map(|a| a.server)
        .or_else(|| std::env::var("KAIROS_SERVER").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| account::DEFAULT_SERVER.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Held while staging and swapping, so the daemon and the CLI never update at the same time.
pub struct UpdateLock(#[allow(dead_code)] File);

pub fn lock(data_dir: &Path) -> Result<Option<UpdateLock>> {
    fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let path = data_dir.join("update.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(UpdateLock(file))),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(e)) => Err(e).with_context(|| format!("locking {}", path.display())),
    }
}

/// Downloads land here before they are checked and swapped in: next to the install folder, so the
/// final rename stays on one file system.
pub fn staging_dir(install_dir: &Path) -> PathBuf {
    install_dir.with_file_name(".kairos-update")
}

fn old_name(dest: &Path) -> PathBuf {
    let stem = dest.file_stem().unwrap_or_default().to_string_lossy();
    match dest.extension() {
        Some(ext) => dest.with_file_name(format!("{stem}.old.{}", ext.to_string_lossy())),
        None => dest.with_file_name(format!("{stem}.old")),
    }
}

/// How many times a just-installed kairosd may start without staying up before the one it replaced is put back.
const TRIAL_STARTS: u32 = 3;

/// Written when an update is swapped in and removed (with the binaries it set aside) once the new kairosd has stayed
/// up: while it is there, the previous version can still be put back.
#[derive(Debug, Serialize, Deserialize)]
struct Trial {
    version: String,
    starts: u32,
    /// Installed file name and the name its previous version was set aside under.
    backups: Vec<(String, String)>,
}

fn trial_path(install_dir: &Path) -> PathBuf {
    install_dir.join("kairos.old-trial.json")
}

fn trial(install_dir: &Path) -> Option<Trial> {
    serde_json::from_str(&fs::read_to_string(trial_path(install_dir)).ok()?).ok()
}

fn skip_path(data_dir: &Path) -> PathBuf {
    data_dir.join("update-skip")
}

/// The version that was put back after it would not stay up. The updater leaves it alone.
pub fn skipped(data_dir: &Path) -> Option<Version> {
    Version::parse(fs::read_to_string(skip_path(data_dir)).ok()?.trim()).ok()
}

/// The kairosd the last update set aside, while it can still be put back.
pub fn previous_daemon(install_dir: &Path, target: &str) -> Option<PathBuf> {
    let daemon = &binaries(target)[1];
    let name = trial(install_dir)?.backups.into_iter().find(|b| b.0 == *daemon)?.1;
    Some(install_dir.join(name)).filter(|p| p.is_file())
}

/// A starting kairosd calls this once it knows it is the only one. True when it put the previous version back because
/// this one kept starting without staying up: the caller starts that one and exits.
pub fn begin(install_dir: &Path, data_dir: &Path, running: &str) -> bool {
    let Some(mut t) = trial(install_dir) else { return false };
    if t.version != running {
        let _ = fs::remove_file(trial_path(install_dir));
        return false;
    }
    if t.starts >= TRIAL_STARTS {
        return roll_back(install_dir, data_dir);
    }
    t.starts += 1;
    let _ = fs::write(trial_path(install_dir), serde_json::to_string(&t).unwrap_or_default());
    false
}

/// Puts back the binaries the last update set aside and marks its version to be skipped. False when there is nothing
/// to put back.
pub fn roll_back(install_dir: &Path, data_dir: &Path) -> bool {
    let Some(t) = trial(install_dir) else { return false };
    let _ = fs::remove_file(trial_path(install_dir));
    let plain = |n: &str| !n.is_empty() && !n.contains(['/', '\\']) && n != "." && n != "..";
    if t.backups.is_empty() || !t.backups.iter().all(|(name, old)| plain(name) && plain(old) && install_dir.join(old).is_file()) {
        return false;
    }
    let mut done = Vec::new();
    for (name, old) in &t.backups {
        match restore(&install_dir.join(name), &install_dir.join(old)) {
            Ok(bad) => done.push((name, old, bad)),
            Err(_) => {
                for (name, old, bad) in done.into_iter().rev() {
                    let _ = fs::rename(install_dir.join(name), install_dir.join(old));
                    if let Some(bad) = bad {
                        let _ = fs::rename(bad, install_dir.join(name));
                    }
                }
                return false;
            }
        }
    }
    let _ = fs::write(skip_path(data_dir), &t.version);
    true
}

#[cfg(not(windows))]
fn restore(dest: &Path, old: &Path) -> io::Result<Option<PathBuf>> {
    fs::rename(old, dest).map(|()| None)
}

// The running exe is renamed out of the way first; cleanup removes it later.
#[cfg(windows)]
fn restore(dest: &Path, old: &Path) -> io::Result<Option<PathBuf>> {
    let stem = dest.file_stem().unwrap_or_default().to_string_lossy();
    let bad = dest.with_file_name(format!("{stem}.old-bad-{}.exe", std::process::id()));
    let _ = fs::remove_file(&bad);
    let moved = fs::rename(dest, &bad).is_ok();
    match fs::rename(old, dest) {
        Ok(()) => Ok(moved.then_some(bad)),
        Err(e) => {
            if moved {
                let _ = fs::rename(&bad, dest);
            }
            Err(e)
        }
    }
}

/// Removes what an update leaves behind, the previous binaries included, so nothing can be put back after it. Call it
/// while holding the update lock.
pub fn cleanup(install_dir: &Path) {
    let _ = fs::remove_dir_all(staging_dir(install_dir));
    for e in fs::read_dir(install_dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("kairos.old") || name.starts_with("kairosd.old") {
            let _ = fs::remove_file(e.path());
        }
    }
}

pub type Fetch<'a> = dyn FnMut(&str) -> Result<Box<dyn Read>> + 'a;

/// On any error the installed files stay as they were. The caller holds the update lock.
pub fn install(release: &Release, target: &str, install_dir: &Path, fetch: &mut Fetch) -> Result<()> {
    let staging = staging_dir(install_dir);
    let names = binaries(target);
    let result = stage(&staging, release, target, fetch)
        .and_then(|()| smoke_test(&staging.join(&names[1]), &release.version))
        .and_then(|()| swap(&staging, install_dir, &names, &release.version.to_string()));
    let _ = fs::remove_dir_all(&staging);
    result
}

fn stage(staging: &Path, release: &Release, target: &str, fetch: &mut Fetch) -> Result<()> {
    let files = &release
        .manifest
        .targets
        .get(target)
        .with_context(|| format!("release {} has no download for {target}", release.version))?
        .files;
    match fs::remove_dir_all(staging) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("clearing {}", staging.display())),
    }
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut dir = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(staging).with_context(|| format!("creating {}", staging.display()))?;
    for name in binaries(target) {
        let want = files.get(&name).with_context(|| format!("release {} is missing {name} for {target}", release.version))?;
        let body = fetch(&format!("{}/{target}/{name}", release.manifest.version))
            .with_context(|| format!("downloading {name}"))?;
        download(body, &staging.join(&name), want).with_context(|| format!("downloading {name}"))?;
    }
    Ok(())
}

/// Writes `body` to a new file, checking size and SHA-256 as it streams. The file is never reopened,
/// so what was checked is what gets run and swapped in.
fn download(mut body: impl Read, path: &Path, want: &FileInfo) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o755);
    let mut file = opts.open(path).with_context(|| format!("creating {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        size += n as u64;
        ensure!(size <= want.size, "the download is larger than the release says ({} bytes)", want.size);
        hash.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }
    ensure!(size == want.size, "the download stopped early ({size} of {} bytes)", want.size);
    ensure!(hex(&hash.finalize()) == want.sha256, "the download does not match the release checksum");
    file.sync_all()?;
    Ok(())
}

pub fn binary_version(exe: &Path) -> Result<String> {
    let mut cmd = Command::new(exe);
    cmd.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
    let mut tries = 0;
    let mut child = loop {
        match cmd.spawn() {
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy && tries < 20 => {
                tries += 1;
                thread::sleep(Duration::from_millis(50));
            }
            r => break r.with_context(|| format!("running {}", exe.display()))?,
        }
    };
    let mut out = child.stdout.take().expect("piped stdout");
    let reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = (&mut out).take(4096).read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + VERSION_TIMEOUT;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{} did not answer --version in time", exe.display());
        }
        thread::sleep(Duration::from_millis(25));
    };
    let text = reader.join().unwrap_or_default();
    ensure!(status.success(), "{} --version failed ({status})", exe.display());
    Ok(text.trim().to_string())
}

pub fn installed_version(daemon: &Path) -> Option<Version> {
    let text = binary_version(daemon).ok()?;
    Version::parse(text.strip_prefix("kairosd ")?).ok()
}

fn smoke_test(daemon: &Path, version: &Version) -> Result<()> {
    let said = binary_version(daemon).context("the new kairosd does not start on this computer")?;
    ensure!(said == format!("kairosd {version}"), "the new kairosd reports {said:?} instead of kairosd {version}");
    Ok(())
}

fn swap(staging: &Path, install_dir: &Path, names: &[String], version: &str) -> Result<()> {
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for name in names {
        let dest = install_dir.join(name);
        let step = set_aside(&dest).and_then(|backup| match fs::rename(staging.join(name), &dest) {
            Ok(()) => Ok(backup),
            Err(e) => {
                put_back(&dest, backup.as_deref());
                Err(e)
            }
        });
        match step {
            Ok(backup) => done.push((dest, backup)),
            Err(e) => {
                for (dest, backup) in done.iter().rev() {
                    put_back(dest, backup.as_deref());
                }
                return Err(e).with_context(|| format!("replacing {}", dest.display()));
            }
        }
    }
    let file = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    let backups = done.iter().filter_map(|(dest, backup)| Some((file(dest)?, file(backup.as_deref()?)?))).collect();
    let trial = Trial { version: version.to_string(), starts: 0, backups };
    let _ = fs::write(trial_path(install_dir), serde_json::to_string(&trial).unwrap_or_default());
    Ok(())
}

#[cfg(unix)]
fn set_aside(dest: &Path) -> io::Result<Option<PathBuf>> {
    if fs::symlink_metadata(dest).is_err() {
        return Ok(None);
    }
    let old = old_name(dest);
    let _ = fs::remove_file(&old);
    if fs::hard_link(dest, &old).is_err() {
        fs::copy(dest, &old)?;
    }
    Ok(Some(old))
}

// A running exe cannot be replaced on Windows, but it can be renamed.
#[cfg(windows)]
fn set_aside(dest: &Path) -> io::Result<Option<PathBuf>> {
    if fs::symlink_metadata(dest).is_err() {
        return Ok(None);
    }
    let mut old = old_name(dest);
    if fs::remove_file(&old).is_err() && old.exists() {
        let stem = dest.file_stem().unwrap_or_default().to_string_lossy();
        old = dest.with_file_name(format!("{stem}.old-{}.exe", std::process::id()));
        let _ = fs::remove_file(&old);
    }
    fs::rename(dest, &old)?;
    Ok(Some(old))
}

fn put_back(dest: &Path, backup: Option<&Path>) {
    match backup {
        Some(b) => {
            let _ = fs::rename(b, dest);
        }
        None => {
            let _ = fs::remove_file(dest);
        }
    }
}

#[cfg(feature = "client")]
pub struct Client {
    http: reqwest::blocking::Client,
    server: String,
    keys: Vec<VerifyingKey>,
}

#[cfg(feature = "client")]
impl Client {
    pub fn new(server: &str) -> Result<Client> {
        Client::with_keys(server, trusted_keys())
    }

    pub fn with_keys(server: &str, keys: Vec<VerifyingKey>) -> Result<Client> {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(15 * 60))
            .user_agent(concat!("kairos/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Client { http, server: server.trim_end_matches('/').to_string(), keys })
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    fn get(&self, path: &str) -> Result<reqwest::blocking::Response> {
        let url = format!("{}/download/{path}", self.server);
        let res = self.http.get(&url).send().with_context(|| format!("could not reach {}", self.server))?;
        ensure!(res.status().is_success(), "{url} answered {}", res.status());
        Ok(res)
    }

    fn small(&self, path: &str, max: u64) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.get(path)?.take(max + 1).read_to_end(&mut buf)?;
        ensure!(buf.len() as u64 <= max, "{path} is larger than expected");
        Ok(buf)
    }

    pub fn latest(&self) -> Result<Release> {
        let manifest = self.small(&format!("latest/{MANIFEST}"), MAX_MANIFEST)?;
        let sig = self.small(&format!("latest/{SIGNATURE}"), MAX_SIGNATURE)?;
        Release::verify(&manifest, &sig, &self.keys)
    }

    pub fn install(&self, release: &Release, target: &str, install_dir: &Path) -> Result<()> {
        install(release, target, install_dir, &mut |path| Ok(Box::new(self.get(path)?) as Box<dyn Read>))
    }
}

/// A development build never updates itself.
pub fn running_installed() -> Option<PathBuf> {
    let dir = paths::install_dir()?;
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    (exe.parent()? == dir.canonicalize().ok()?).then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    pub fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn info(bytes: &[u8]) -> FileInfo {
        FileInfo { sha256: sha256_hex(bytes), size: bytes.len() as u64 }
    }

    fn manifest(version: &str, files: &[(&str, &[u8])]) -> Manifest {
        let files = files.iter().map(|(n, b)| (n.to_string(), info(b))).collect();
        Manifest {
            version: version.into(),
            published_at: "2026-11-19T00:00:00Z".into(),
            targets: BTreeMap::from([("linux-x86_64".to_string(), Target { files })]),
        }
    }

    fn release(m: &Manifest) -> Release {
        let bytes = m.to_bytes();
        Release::verify(&bytes, sign(&bytes, &key(1)).as_bytes(), &[key(1).verifying_key()]).unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kairos-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();
        dir
    }

    #[test]
    fn compiled_keys_parse() {
        assert_eq!(trusted_keys().len(), TRUSTED_KEYS.len());
    }

    #[test]
    fn target_mapping() {
        assert_eq!(target_for_triple("x86_64-pc-windows-msvc"), Some("windows-x86_64"));
        assert_eq!(target_for_triple("x86_64-pc-windows-gnu"), Some("windows-x86_64"));
        assert_eq!(target_for_triple("aarch64-apple-darwin"), Some("macos-aarch64"));
        assert_eq!(target_for_triple("x86_64-apple-darwin"), Some("macos-x86_64"));
        assert_eq!(target_for_triple("x86_64-unknown-linux-musl"), Some("linux-x86_64"));
        assert_eq!(target_for_triple("aarch64-unknown-linux-musl"), Some("linux-aarch64"));
        assert_eq!(target_for_triple("aarch64-unknown-linux-gnu"), Some("linux-aarch64"));
        assert_eq!(target_for_triple("aarch64-pc-windows-msvc"), None);
        assert_eq!(target_for_triple("i686-unknown-linux-gnu"), None);
        assert_eq!(target_for_triple("wasm32-unknown-unknown"), None);
        for t in TARGETS {
            assert!(plain_name(t));
        }
        let me = current_target();
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(me, Some("linux-x86_64"));
        }
        assert_eq!(binaries("windows-x86_64"), ["kairos.exe", "kairosd.exe"]);
        assert_eq!(binaries("macos-aarch64"), ["kairos", "kairosd"]);
    }

    #[test]
    fn manifest_bytes_are_stable() {
        let m = manifest("1.2.3", &[("kairosd", b"d"), ("kairos", b"c")]);
        let text = String::from_utf8(m.to_bytes()).unwrap();
        assert!(text.starts_with("{\n  \"version\": \"1.2.3\",\n  \"published_at\""));
        assert!(text.find("\"kairos\"").unwrap() < text.find("\"kairosd\"").unwrap());
        assert!(text.ends_with("}\n"));
        assert_eq!(m.to_bytes(), m.to_bytes());
    }

    #[test]
    fn verify_accepts_only_trusted_untampered_manifests() {
        let m = manifest("1.2.3", &[("kairos", b"c"), ("kairosd", b"d")]);
        let bytes = m.to_bytes();
        let sig = sign(&bytes, &key(1));
        let keys = [key(2).verifying_key(), key(1).verifying_key()];
        assert_eq!(Release::verify(&bytes, sig.as_bytes(), &keys).unwrap().manifest, m);

        let err = |b: &[u8], s: &[u8], k: &[VerifyingKey]| Release::verify(b, s, k).unwrap_err().to_string();
        assert!(err(&bytes, sig.as_bytes(), &[key(2).verifying_key()]).contains("not signed"));
        let mut tampered = bytes.clone();
        tampered[20] ^= 1;
        assert!(err(&tampered, sig.as_bytes(), &keys).contains("not signed"));
        let mut sig_raw = B64.decode(sig.trim()).unwrap();
        sig_raw[0] ^= 1;
        assert!(err(&bytes, B64.encode(&sig_raw).as_bytes(), &keys).contains("not signed"));
        assert!(err(&bytes, b"not base64!", &keys).contains("base64"));
        assert!(err(&bytes, B64.encode([0u8; 10]).as_bytes(), &keys).contains("length"));
        assert!(err(&bytes, sig.as_bytes(), &[]).contains("not signed"));
    }

    #[test]
    fn verify_rejects_signed_but_unsafe_manifests() {
        let signed = |m: &Manifest| {
            let b = m.to_bytes();
            Release::verify(&b, sign(&b, &key(1)).as_bytes(), &[key(1).verifying_key()])
        };
        assert!(signed(&manifest("1.2.3+build", &[])).is_err());
        assert!(signed(&manifest("one", &[])).is_err());
        assert!(signed(&manifest("1.2.3", &[("../kairos", b"x")])).is_err());
        let mut m = manifest("1.2.3", &[("kairos", b"x")]);
        m.targets.get_mut("linux-x86_64").unwrap().files.get_mut("kairos").unwrap().sha256 = "AB".repeat(32);
        assert!(signed(&m).is_err());
        let mut m = manifest("1.2.3", &[]);
        m.targets.insert("../x".into(), Target { files: BTreeMap::new() });
        assert!(signed(&m).is_err());
    }

    #[test]
    fn only_strictly_newer_versions() {
        let r = release(&manifest("1.2.3", &[]));
        assert_eq!(r.newer_than("1.2.2").unwrap().map(|v| v.to_string()), Some("1.2.3".into()));
        assert_eq!(r.newer_than("1.2.3").unwrap(), None);
        assert_eq!(r.newer_than("1.10.0").unwrap(), None);
        assert_eq!(r.newer_than("1.2.3-rc.1").unwrap().map(|v| v.to_string()), Some("1.2.3".into()));
        assert!(r.newer_than("garbage").is_err());
    }

    #[test]
    fn download_checks_size_and_hash_while_streaming() {
        let dir = temp("download");
        let want = info(b"hello");
        download(Cursor::new(b"hello".to_vec()), &dir.join("ok"), &want).unwrap();
        assert_eq!(fs::read(dir.join("ok")).unwrap(), b"hello");
        let e = download(Cursor::new(b"hello!".to_vec()), &dir.join("long"), &want).unwrap_err();
        assert!(e.to_string().contains("larger"));
        let e = download(Cursor::new(b"hell".to_vec()), &dir.join("short"), &want).unwrap_err();
        assert!(e.to_string().contains("early"));
        let e = download(Cursor::new(b"jello".to_vec()), &dir.join("bad"), &want).unwrap_err();
        assert!(e.to_string().contains("checksum"));
        assert!(download(Cursor::new(b"hello".to_vec()), &dir.join("ok"), &want).is_err(), "never overwrites");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn script(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\necho \"kairosd {version}\"\n").into_bytes()
    }

    #[cfg(unix)]
    fn serve(files: Vec<(String, Vec<u8>)>) -> impl FnMut(&str) -> Result<Box<dyn Read>> {
        move |path: &str| {
            let (_, body) = files.iter().find(|(p, _)| p == path).with_context(|| format!("404 {path}"))?;
            Ok(Box::new(Cursor::new(body.clone())) as Box<dyn Read>)
        }
    }

    #[cfg(unix)]
    #[test]
    fn install_swaps_both_binaries_and_cleans_up() {
        let dir = temp("swap");
        let bin = dir.join("bin");
        fs::write(bin.join("kairos"), b"old cli").unwrap();
        fs::write(bin.join("kairosd"), b"old daemon").unwrap();
        let daemon = script("2.0.0");
        let r = release(&manifest("2.0.0", &[("kairos", b"new cli"), ("kairosd", &daemon)]));
        let mut fetch = serve(vec![
            ("2.0.0/linux-x86_64/kairos".into(), b"new cli".to_vec()),
            ("2.0.0/linux-x86_64/kairosd".into(), daemon.clone()),
        ]);
        install(&r, "linux-x86_64", &bin, &mut fetch).unwrap();
        assert_eq!(fs::read(bin.join("kairos")).unwrap(), b"new cli");
        assert_eq!(fs::read(bin.join("kairosd")).unwrap(), daemon);
        assert_eq!(installed_version(&bin.join("kairosd")).unwrap().to_string(), "2.0.0");
        assert!(!staging_dir(&bin).exists());
        let files = |bin: &Path| {
            let mut left: Vec<_> = fs::read_dir(bin).unwrap().map(|e| e.unwrap().file_name()).collect();
            left.sort();
            left
        };
        assert_eq!(files(&bin), ["kairos", "kairos.old", "kairos.old-trial.json", "kairosd", "kairosd.old"]);
        assert_eq!(previous_daemon(&bin, "linux-x86_64"), Some(bin.join("kairosd.old")));
        cleanup(&bin);
        assert_eq!(files(&bin), ["kairos", "kairosd"]);
        assert_eq!(previous_daemon(&bin, "linux-x86_64"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_version_that_keeps_starting_without_staying_up_is_replaced_by_the_one_before() {
        let dir = temp("trial");
        let (bin, data) = (dir.join("bin"), dir.join("data"));
        fs::create_dir_all(&data).unwrap();
        let old = script("1.0.0");
        fs::write(bin.join("kairos"), b"old cli").unwrap();
        fs::write(bin.join("kairosd"), &old).unwrap();
        let daemon = script("2.0.0");
        let r = release(&manifest("2.0.0", &[("kairos", b"new cli"), ("kairosd", &daemon)]));
        let files = vec![("2.0.0/linux-x86_64/kairos".into(), b"new cli".to_vec()), ("2.0.0/linux-x86_64/kairosd".into(), daemon.clone())];
        install(&r, "linux-x86_64", &bin, &mut serve(files.clone())).unwrap();

        for _ in 0..TRIAL_STARTS {
            assert!(!begin(&bin, &data, "2.0.0"));
        }
        assert_eq!(fs::read(bin.join("kairosd")).unwrap(), daemon);
        assert_eq!(skipped(&data), None);
        assert!(begin(&bin, &data, "2.0.0"));
        assert_eq!(fs::read(bin.join("kairos")).unwrap(), b"old cli");
        assert_eq!(fs::read(bin.join("kairosd")).unwrap(), old);
        assert_eq!(skipped(&data).unwrap().to_string(), "2.0.0");
        let mut left: Vec<_> = fs::read_dir(&bin).unwrap().map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["kairos", "kairosd"]);
        assert!(!begin(&bin, &data, "1.0.0"));

        install(&r, "linux-x86_64", &bin, &mut serve(files)).unwrap();
        assert!(!begin(&bin, &data, "2.0.0"));
        cleanup(&bin);
        for _ in 0..5 {
            assert!(!begin(&bin, &data, "2.0.0"));
        }
        assert_eq!(fs::read(bin.join("kairosd")).unwrap(), daemon);
        assert!(!roll_back(&bin, &data));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_trial_left_by_another_version_or_without_its_files_puts_nothing_back() {
        let dir = temp("stale");
        let (bin, data) = (dir.join("bin"), dir.join("data"));
        fs::create_dir_all(&data).unwrap();
        fs::write(bin.join("kairosd"), b"now").unwrap();
        let write = |t: &Trial| fs::write(trial_path(&bin), serde_json::to_string(t).unwrap()).unwrap();
        write(&Trial { version: "2.0.0".into(), starts: 9, backups: vec![("kairosd".into(), "kairosd.old".into())] });
        assert!(!begin(&bin, &data, "3.0.0"));
        assert!(!trial_path(&bin).exists());
        write(&Trial { version: "2.0.0".into(), starts: 9, backups: vec![("kairosd".into(), "kairosd.old".into())] });
        assert!(!begin(&bin, &data, "2.0.0"));
        fs::write(dir.join("elsewhere"), b"x").unwrap();
        write(&Trial { version: "2.0.0".into(), starts: 9, backups: vec![("kairosd".into(), "../elsewhere".into())] });
        assert!(!begin(&bin, &data, "2.0.0"));
        assert_eq!(fs::read(bin.join("kairosd")).unwrap(), b"now");
        assert_eq!(skipped(&data), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn failed_smoke_test_leaves_the_old_binaries() {
        let dir = temp("smoke");
        let bin = dir.join("bin");
        fs::write(bin.join("kairos"), b"old cli").unwrap();
        fs::write(bin.join("kairosd"), b"old daemon").unwrap();
        for daemon in [script("1.9.9"), b"#!/bin/sh\nexit 3\n".to_vec(), b"\x7fELF garbage".to_vec()] {
            let r = release(&manifest("2.0.0", &[("kairos", b"new cli"), ("kairosd", &daemon)]));
            let mut fetch = serve(vec![
                ("2.0.0/linux-x86_64/kairos".into(), b"new cli".to_vec()),
                ("2.0.0/linux-x86_64/kairosd".into(), daemon.clone()),
            ]);
            let e = install(&r, "linux-x86_64", &bin, &mut fetch).unwrap_err();
            assert!(format!("{e:#}").contains("kairosd"), "{e:#}");
            assert_eq!(fs::read(bin.join("kairos")).unwrap(), b"old cli");
            assert_eq!(fs::read(bin.join("kairosd")).unwrap(), b"old daemon");
            assert!(!staging_dir(&bin).exists());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn bad_download_or_missing_file_installs_nothing() {
        let dir = temp("baddl");
        let bin = dir.join("bin");
        fs::write(bin.join("kairos"), b"old cli").unwrap();
        fs::write(bin.join("kairosd"), b"old daemon").unwrap();
        let daemon = script("2.0.0");
        let r = release(&manifest("2.0.0", &[("kairos", b"new cli"), ("kairosd", &daemon)]));
        let mut fetch = serve(vec![
            ("2.0.0/linux-x86_64/kairos".into(), b"new cli".to_vec()),
            ("2.0.0/linux-x86_64/kairosd".into(), script("6.6.6")),
        ]);
        let e = install(&r, "linux-x86_64", &bin, &mut fetch).unwrap_err();
        assert!(format!("{e:#}").contains("checksum"), "{e:#}");
        let r = release(&manifest("2.0.0", &[("kairos", b"new cli")]));
        let e = install(&r, "linux-x86_64", &bin, &mut fetch).unwrap_err();
        assert!(e.to_string().contains("missing kairosd"), "{e:#}");
        let e = install(&r, "macos-aarch64", &bin, &mut fetch).unwrap_err();
        assert!(e.to_string().contains("no download for macos-aarch64"), "{e:#}");
        assert_eq!(fs::read(bin.join("kairos")).unwrap(), b"old cli");
        assert_eq!(fs::read(bin.join("kairosd")).unwrap(), b"old daemon");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn swap_rolls_back_when_a_later_file_fails() {
        let dir = temp("rollback");
        let bin = dir.join("bin");
        let staging = staging_dir(&bin);
        fs::create_dir_all(&staging).unwrap();
        fs::write(bin.join("kairos"), b"old cli").unwrap();
        fs::write(bin.join("kairosd"), b"old daemon").unwrap();
        fs::write(staging.join("kairos"), b"new cli").unwrap();
        let names = binaries("linux-x86_64");
        assert!(swap(&staging, &bin, &names, "2.0.0").is_err());
        assert_eq!(fs::read(bin.join("kairos")).unwrap(), b"old cli");
        assert_eq!(fs::read(bin.join("kairosd")).unwrap(), b"old daemon");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_leftovers_only() {
        let dir = temp("cleanup");
        let bin = dir.join("bin");
        for f in ["kairos", "kairosd", "kairos.old", "kairosd.old.exe", "kairosd.old-12.exe", "kairos.old-trial.json", "other.old"] {
            fs::write(bin.join(f), b"x").unwrap();
        }
        fs::create_dir_all(staging_dir(&bin).join("x")).unwrap();
        cleanup(&bin);
        let mut left: Vec<_> = fs::read_dir(&bin).unwrap().map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["kairos", "kairosd", "other.old"]);
        assert!(!staging_dir(&bin).exists());
        assert_eq!(old_name(Path::new("a/kairosd.exe")), Path::new("a/kairosd.old.exe"));
        assert_eq!(old_name(Path::new("a/kairosd")), Path::new("a/kairosd.old"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_lock_is_exclusive() {
        let dir = temp("lock");
        let first = lock(&dir).unwrap();
        assert!(first.is_some());
        assert!(lock(&dir).unwrap().is_none());
        drop(first);
        // A program another test is starting holds a copy of every open file until it is running, the lock's too.
        let free = (0..100).any(|_| lock(&dir).unwrap().is_some() || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            false
        });
        assert!(free);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "client")]
    #[test]
    fn client_fetches_and_verifies_over_http() {
        use std::io::BufRead;
        use std::net::TcpListener;

        let m = manifest("3.0.0", &[("kairos", b"c"), ("kairosd", b"d")]);
        let body = m.to_bytes();
        let sig = sign(&body, &key(1));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let mut stream = stream.unwrap();
                let mut line = String::new();
                io::BufReader::new(&stream).read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let (status, bytes) = match path.as_str() {
                    "/download/latest/manifest.json" => ("200 OK", body.clone()),
                    "/download/latest/manifest.json.sig" => ("200 OK", sig.clone().into_bytes()),
                    _ => ("404 Not Found", Vec::new()),
                };
                let head = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", bytes.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&bytes);
            }
        });
        let server = format!("http://{addr}/");
        let r = Client::with_keys(&server, vec![key(1).verifying_key()]).unwrap().latest().unwrap();
        assert_eq!(r.version.to_string(), "3.0.0");
        let e = Client::with_keys(&server, vec![key(2).verifying_key()]).unwrap().latest().unwrap_err();
        assert!(e.to_string().contains("not signed"));
    }
}
