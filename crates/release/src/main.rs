
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, ensure, Context, Result};
use kairos_core::update::{self, FileInfo, Manifest, Release, Target, MANIFEST, SIGNATURE, SUMS};

const USAGE: &str = "usage: kairos-release sign --dist DIR --version X.Y.Z   (seed from KAIROS_SIGNING_KEY)\n       kairos-release verify --dist DIR [--pubkey BASE64]... [--version X.Y.Z]\n       kairos-release pubkey   (public key for KAIROS_SIGNING_KEY)";

fn entries(dir: &Path) -> Result<BTreeMap<String, fs::Metadata>> {
    let mut out = BTreeMap::new();
    for e in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let e = e?;
        let name = e.file_name().into_string().map_err(|n| anyhow::anyhow!("{n:?} is not a UTF-8 name"))?;
        out.insert(name, fs::symlink_metadata(e.path())?);
    }
    Ok(out)
}

fn file_info(path: &Path) -> Result<FileInfo> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(FileInfo { sha256: update::sha256_hex(&bytes), size: bytes.len() as u64 })
}

fn sign(dist: &Path, version: &str, key: &update::SigningKey, published_at: String) -> Result<Manifest> {
    update::release_version(version)?;
    let mut targets = BTreeMap::new();
    for (name, meta) in entries(dist)? {
        if name == MANIFEST || name == SIGNATURE {
            continue;
        }
        ensure!(meta.is_dir(), "unexpected file {name} in {}", dist.display());
        ensure!(update::TARGETS.contains(&name.as_str()), "{name} is not a release target ({})", update::TARGETS.join(", "));
        let dir = dist.join(&name);
        let mut files = BTreeMap::new();
        for (file, meta) in entries(&dir)? {
            if file == SUMS {
                continue;
            }
            ensure!(meta.is_file(), "{name}/{file} is not a regular file");
            ensure!(update::plain_name(&file), "{name}/{file} has a name that cannot be served");
            files.insert(file.clone(), file_info(&dir.join(&file))?);
        }
        for bin in update::binaries(&name) {
            ensure!(files.contains_key(&bin), "{name} is missing {bin}");
        }
        fs::write(dir.join(SUMS), update::sums_file(&files))?;
        targets.insert(name, Target { files });
    }
    ensure!(!targets.is_empty(), "{} has no target folders", dist.display());
    let manifest = Manifest { version: version.to_string(), published_at, targets };
    let bytes = manifest.to_bytes();
    fs::write(dist.join(MANIFEST), &bytes)?;
    fs::write(dist.join(SIGNATURE), update::sign(&bytes, key))?;
    Ok(manifest)
}

fn verify(dist: &Path, keys: &[update::VerifyingKey], version: Option<&str>) -> Result<Manifest> {
    let bytes = fs::read(dist.join(MANIFEST)).with_context(|| format!("reading {MANIFEST}"))?;
    let sig = fs::read(dist.join(SIGNATURE)).with_context(|| format!("reading {SIGNATURE}"))?;
    let m = Release::verify(&bytes, &sig, keys)?.manifest;
    if let Some(v) = version {
        ensure!(m.version == v, "the manifest is for version {}, not {v}", m.version);
    }
    let mut dirs = BTreeSet::new();
    for (name, meta) in entries(dist)? {
        if name == MANIFEST || name == SIGNATURE {
            continue;
        }
        ensure!(meta.is_dir(), "unexpected file {name}");
        dirs.insert(name);
    }
    let listed: BTreeSet<String> = m.targets.keys().cloned().collect();
    if let Some(extra) = dirs.difference(&listed).next() {
        bail!("{extra} is not in the manifest");
    }
    if let Some(missing) = listed.difference(&dirs).next() {
        bail!("{missing} is in the manifest but missing");
    }
    for (target, t) in &m.targets {
        let dir = dist.join(target);
        for bin in update::binaries(target) {
            ensure!(t.files.contains_key(&bin), "the manifest has no {target}/{bin}");
        }
        let mut found = BTreeSet::new();
        for (file, meta) in entries(&dir)? {
            if file == SUMS {
                continue;
            }
            ensure!(meta.is_file(), "{target}/{file} is not a regular file");
            ensure!(t.files.contains_key(&file), "{target}/{file} is not in the manifest");
            found.insert(file);
        }
        for (file, want) in &t.files {
            ensure!(found.contains(file), "{target}/{file} is missing");
            let got = file_info(&dir.join(file))?;
            ensure!(got.size == want.size, "{target}/{file} is {} bytes, the manifest says {}", got.size, want.size);
            ensure!(got.sha256 == want.sha256, "{target}/{file} does not match its checksum");
        }
        let sums = fs::read_to_string(dir.join(SUMS)).with_context(|| format!("reading {target}/{SUMS}"))?;
        ensure!(sums == update::sums_file(&t.files), "{target}/{SUMS} does not match the manifest");
    }
    Ok(m)
}

fn seed() -> Result<update::SigningKey> {
    let seed = std::env::var("KAIROS_SIGNING_KEY").ok().filter(|s| !s.trim().is_empty());
    update::signing_key(&seed.context("KAIROS_SIGNING_KEY is not set")?)
}

fn run(args: Vec<String>) -> Result<()> {
    let mut it = args.into_iter();
    let cmd = it.next().unwrap_or_default();
    let mut dist: Option<PathBuf> = None;
    let mut version = None;
    let mut keys = Vec::new();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--dist" => dist = Some(value()?.into()),
            "--version" => version = Some(value()?),
            "--pubkey" => keys.push(update::parse_key(&value()?)?),
            _ => bail!("unknown option {a:?}\n{USAGE}"),
        }
    }
    match cmd.as_str() {
        "sign" => {
            let (Some(dist), Some(version)) = (dist, version) else { bail!("{USAGE}") };
            let published_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let m = sign(&dist, &version, &seed()?, published_at)?;
            println!("signed kairos {} for {}", m.version, m.targets.keys().cloned().collect::<Vec<_>>().join(", "));
        }
        "verify" => {
            let dist = dist.with_context(|| USAGE.to_string())?;
            if keys.is_empty() {
                keys = update::trusted_keys();
            }
            let m = verify(&dist, &keys, version.as_deref())?;
            println!("ok kairos {} for {}", m.version, m.targets.keys().cloned().collect::<Vec<_>>().join(", "));
        }
        "pubkey" => println!("{}", update::encode_key(&seed()?.verifying_key())),
        _ => bail!("{USAGE}"),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kairos-release: {e:#}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> update::SigningKey {
        update::SigningKey::from_bytes(&[n; 32])
    }

    fn dist(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kairos-release-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for (t, files) in [("linux-x86_64", ["kairos", "kairosd"]), ("windows-x86_64", ["kairos.exe", "kairosd.exe"])] {
            fs::create_dir_all(d.join(t)).unwrap();
            for f in files {
                fs::write(d.join(t).join(f), format!("{t}/{f} bytes")).unwrap();
            }
        }
        d
    }

    fn signed(name: &str) -> PathBuf {
        let d = dist(name);
        sign(&d, "1.4.0", &key(1), "2026-11-19T00:00:00Z".into()).unwrap();
        d
    }

    fn check(d: &Path) -> Result<Manifest> {
        verify(d, &[key(1).verifying_key()], Some("1.4.0"))
    }

    fn fails(d: &Path, needle: &str) {
        let e = format!("{:#}", check(d).unwrap_err());
        assert!(e.contains(needle), "{e}");
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn round_trip() {
        let d = signed("ok");
        let m = check(&d).unwrap();
        assert_eq!(m.version, "1.4.0");
        assert_eq!(m.targets.len(), 2);
        let sums = fs::read_to_string(d.join("linux-x86_64/SHA256SUMS")).unwrap();
        assert_eq!(sums.lines().count(), 2);
        assert!(sums.ends_with("  kairosd\n"));
        assert!(verify(&d, &[key(1).verifying_key()], Some("1.5.0")).is_err());
        let again = sign(&d, "1.4.0", &key(1), "2026-11-19T00:00:00Z".into()).unwrap();
        assert_eq!(again, m, "re-signing a signed tree is stable");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn bad_signature() {
        let d = signed("sig");
        let sig = fs::read_to_string(d.join(SIGNATURE)).unwrap();
        let flipped = if sig.starts_with('A') { sig.replacen('A', "B", 1) } else { format!("A{}", &sig[1..]) };
        fs::write(d.join(SIGNATURE), flipped).unwrap();
        fails(&d, "not signed");
    }

    #[test]
    fn edited_manifest() {
        let d = signed("manifest");
        let text = fs::read_to_string(d.join(MANIFEST)).unwrap().replace("1.4.0", "1.4.1");
        fs::write(d.join(MANIFEST), text).unwrap();
        fails(&d, "not signed");
    }

    #[test]
    fn unknown_key() {
        let d = signed("key");
        let e = verify(&d, &[key(2).verifying_key()], None).unwrap_err();
        assert!(e.to_string().contains("not signed"));
        let e = verify(&d, &update::trusted_keys(), None).unwrap_err();
        assert!(e.to_string().contains("not signed"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn wrong_hash() {
        let d = signed("hash");
        fs::write(d.join("linux-x86_64/kairosd"), "linux-x86_64/kairosd BYTES").unwrap();
        fails(&d, "checksum");
    }

    #[test]
    fn wrong_size() {
        let d = signed("size");
        fs::write(d.join("windows-x86_64/kairos.exe"), "longer than it was before").unwrap();
        fails(&d, "bytes, the manifest says");
    }

    #[test]
    fn extra_file() {
        let d = signed("extra");
        fs::write(d.join("linux-x86_64/evil"), "x").unwrap();
        fails(&d, "not in the manifest");
    }

    #[test]
    fn missing_file() {
        let d = signed("missing");
        fs::remove_file(d.join("linux-x86_64/kairos")).unwrap();
        fails(&d, "kairos is missing");
    }

    #[test]
    fn extra_and_missing_target() {
        let d = signed("extra-target");
        fs::create_dir_all(d.join("macos-aarch64")).unwrap();
        fails(&d, "macos-aarch64 is not in the manifest");
        let d = signed("missing-target");
        fs::remove_dir_all(d.join("windows-x86_64")).unwrap();
        fails(&d, "windows-x86_64 is in the manifest but missing");
    }

    #[test]
    fn edited_sums() {
        let d = signed("sums");
        fs::write(d.join("linux-x86_64/SHA256SUMS"), "").unwrap();
        fails(&d, "SHA256SUMS does not match");
    }

    #[test]
    fn sign_refuses_bad_trees() {
        let d = dist("bad-version");
        assert!(sign(&d, "1.4", &key(1), String::new()).is_err());
        assert!(sign(&d, "1.4.0+meta", &key(1), String::new()).is_err());
        fs::create_dir_all(d.join("linux-riscv64")).unwrap();
        assert!(sign(&d, "1.4.0", &key(1), String::new()).unwrap_err().to_string().contains("not a release target"));
        fs::remove_dir_all(d.join("linux-riscv64")).unwrap();
        fs::remove_file(d.join("windows-x86_64/kairosd.exe")).unwrap();
        assert!(sign(&d, "1.4.0", &key(1), String::new()).unwrap_err().to_string().contains("missing kairosd.exe"));
        let _ = fs::remove_dir_all(&d);
    }
}
