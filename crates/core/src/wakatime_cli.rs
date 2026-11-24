//! The person's wakatime-cli, which kairosd runs to see what Hackatime would get from agents' logs, and the official one for
//! people who don't have it yet.

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::update::sha256_hex;

/// The wakatime-cli Kairos gets when there is none. The zips' checksums live here instead of being read from the release, so a
/// file changed after Kairos was built is refused.
pub const VERSION: &str = "v2.28.4";
const SUMS: [(&str, &str); 6] = [
    ("darwin-amd64", "e8fed4233155e55a33d06f2bb365abe6f5bf6789b3753dea522ef3dea8bd6884"),
    ("darwin-arm64", "e70db3d824d73939bf4b79146dd3679ff429527b925a10f3d82a7291dec03c6d"),
    ("linux-amd64", "e1dfa9fc4960cb0ebed90ee978c921b8ded9c61ffa63e750ddabd2fedb570345"),
    ("linux-arm64", "77ee19b58880d6c91037584b2f505c3f3fd4e6d550b3b306e72cbfceb3558355"),
    ("windows-amd64", "76f90160dd5268f3b74be2aa75323c9387e79db0874cae71269d1d74c88dd661"),
    ("windows-arm64", "d3d1d6df9383160fa7a95b592890d60bc39098e1dd72344609729dcb0ab18454"),
];
const RELEASES: &str = "https://github.com/wakatime/wakatime-cli/releases/download";
const MAX_ZIP: u64 = 32 << 20;

/// wakatime-cli's name for this computer (`GOOS-GOARCH`), for the ones Kairos runs on.
pub fn platform() -> Option<&'static str> {
    platform_of(std::env::consts::OS, std::env::consts::ARCH)
}

fn platform_of(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "x86_64") => "linux-amd64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "x86_64") => "darwin-amd64",
        ("macos", "aarch64") => "darwin-arm64",
        ("windows", "x86_64") => "windows-amd64",
        ("windows", "aarch64") => "windows-arm64",
        _ => return None,
    })
}

fn file_name(platform: &str) -> String {
    let exe = if platform.starts_with("windows") { ".exe" } else { "" };
    format!("wakatime-cli-{platform}{exe}")
}

/// Where editor plugins and Hackatime's setup keep wakatime-cli: `$WAKATIME_HOME` when set, else `~/.wakatime`, as wakatime-cli
/// itself reads it.
pub fn resources_dir() -> Option<PathBuf> {
    match std::env::var_os("WAKATIME_HOME").filter(|h| !h.is_empty()) {
        Some(home) => Some(PathBuf::from(home)),
        None => Some(dirs::home_dir()?.join(".wakatime")),
    }
}

/// The person's wakatime-cli: the one plugins put in [`resources_dir`], else one on the PATH.
pub fn find() -> Option<PathBuf> {
    find_in(&resources_dir()?, std::env::var_os("PATH").as_deref(), platform()?)
}

fn find_in(dir: &Path, path: Option<&OsStr>, platform: &str) -> Option<PathBuf> {
    let exe = if platform.starts_with("windows") { ".exe" } else { "" };
    let plain = format!("wakatime-cli{exe}");
    [dir.join(file_name(platform)), dir.join(&plain)]
        .into_iter()
        .chain(path.into_iter().flat_map(std::env::split_paths).map(|d| d.join(&plain)))
        .find(|f| f.is_file())
}

/// Gets the official wakatime-cli into [`resources_dir`], where plugins look for it, unless one is already there.
pub fn install() -> Result<PathBuf> {
    install_in(&resources_dir().context("cannot find your home folder")?)
}

/// [`install`], into `dir`.
pub fn install_in(dir: &Path) -> Result<PathBuf> {
    let platform = platform().context("wakatime-cli has no build for this computer")?;
    let dest = dir.join(file_name(platform));
    if dest.is_file() {
        return Ok(dest);
    }
    place(&download(platform)?, sum(platform), &file_name(platform), dir)
}

fn download(platform: &str) -> Result<Vec<u8>> {
    let url = format!("{RELEASES}/{VERSION}/wakatime-cli-{platform}.zip");
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("kairos/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building the http client")?;
    let resp = http.get(&url).send().and_then(|r| r.error_for_status()).with_context(|| format!("downloading {url}"))?;
    let mut zip = Vec::new();
    resp.take(MAX_ZIP + 1).read_to_end(&mut zip).with_context(|| format!("downloading {url}"))?;
    if zip.len() as u64 > MAX_ZIP {
        bail!("{url} is larger than expected");
    }
    Ok(zip)
}

fn sum(platform: &str) -> &'static str {
    SUMS.iter().find(|(p, _)| *p == platform).map_or("", |(_, s)| s)
}

fn place(zip: &[u8], sum: &str, name: &str, dir: &Path) -> Result<PathBuf> {
    if sha256_hex(zip) != sum {
        bail!("the downloaded wakatime-cli is not the one Kairos expects, so it was not used");
    }
    let bin = unzip_one(zip, name)?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let dest = dir.join(name);
    let tmp = dir.join(format!("{name}.kairos-tmp"));
    fs::write(&tmp, &bin).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).with_context(|| format!("writing {}", tmp.display()))?;
    }
    fs::rename(&tmp, &dest).with_context(|| format!("writing {}", dest.display()))?;
    Ok(dest)
}

/// The file called `name` in a zip whose checksum was already checked, so only what that zip needs is read: stored or deflated
/// entries, sizes from the central directory (wakatime-cli's zips are written as a stream).
fn unzip_one(zip: &[u8], name: &str) -> Result<Vec<u8>> {
    let u16_at = |i: usize| zip.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).context("the zip is cut short");
    let u32_at = |i: usize| zip.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).context("the zip is cut short");
    let end = (0..=zip.len().saturating_sub(22)).rev().find(|&i| zip[i..].starts_with(b"PK\x05\x06")).context("not a zip")?;
    let mut at = u32_at(end + 16)? as usize;
    for _ in 0..u16_at(end + 10)? {
        if u32_at(at)? != 0x0201_4b50 {
            bail!("the zip's directory is damaged");
        }
        let (method, crc, packed, size) = (u16_at(at + 10)?, u32_at(at + 16)?, u32_at(at + 20)? as usize, u32_at(at + 24)? as usize);
        let (len, extra, comment, local) = (u16_at(at + 28)?, u16_at(at + 30)?, u16_at(at + 32)?, u32_at(at + 42)? as usize);
        let entry = zip.get(at + 46..at + 46 + len).context("the zip is cut short")?;
        at += 46 + len + extra + comment;
        if entry.rsplit(|&b| b == b'/').next() != Some(name.as_bytes()) {
            continue;
        }
        if u32_at(local)? != 0x0403_4b50 {
            bail!("the zip's {name} entry is damaged");
        }
        let start = local + 30 + u16_at(local + 26)? + u16_at(local + 28)?;
        let data = zip.get(start..start + packed).context("the zip is cut short")?;
        let out = match method {
            0 => data.to_vec(),
            8 => {
                let mut out = Vec::with_capacity(size);
                flate2::read::DeflateDecoder::new(data).take(size as u64 + 1).read_to_end(&mut out).context("unpacking the zip")?;
                out
            }
            m => bail!("the zip uses compression {m}, which Kairos can't unpack"),
        };
        let mut check = flate2::Crc::new();
        check.update(&out);
        if out.len() != size || check.sum() != crc {
            bail!("the zip's {name} entry is damaged");
        }
        return Ok(out);
    }
    bail!("{name} is not in the zip")
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let dir = std::env::temp_dir().join(format!("kairos-wakatime-cli-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A zip like Go's archive/zip writes: each entry streamed with its sizes in a data descriptor, the real ones in the directory.
    fn zip_of(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let (mut out, mut dir) = (Vec::new(), Vec::new());
        for (name, body, deflate) in files {
            let packed = if *deflate {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(body).unwrap();
                e.finish().unwrap()
            } else {
                body.to_vec()
            };
            let mut crc = flate2::Crc::new();
            crc.update(body);
            let (method, offset) = (if *deflate { 8u16 } else { 0 }, out.len() as u32);
            out.extend(b"PK\x03\x04");
            out.extend(20u16.to_le_bytes());
            out.extend(8u16.to_le_bytes());
            out.extend(method.to_le_bytes());
            out.extend([0u8; 4]);
            out.extend([0u8; 12]);
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend(&packed);
            out.extend(b"PK\x07\x08");
            out.extend(crc.sum().to_le_bytes());
            out.extend((packed.len() as u32).to_le_bytes());
            out.extend((body.len() as u32).to_le_bytes());
            dir.extend(b"PK\x01\x02");
            dir.extend(20u16.to_le_bytes());
            dir.extend(20u16.to_le_bytes());
            dir.extend(8u16.to_le_bytes());
            dir.extend(method.to_le_bytes());
            dir.extend([0u8; 4]);
            dir.extend(crc.sum().to_le_bytes());
            dir.extend((packed.len() as u32).to_le_bytes());
            dir.extend((body.len() as u32).to_le_bytes());
            dir.extend((name.len() as u16).to_le_bytes());
            dir.extend([0u8; 12]);
            dir.extend(offset.to_le_bytes());
            dir.extend(name.as_bytes());
        }
        let (start, size) = (out.len() as u32, dir.len() as u32);
        out.extend(&dir);
        out.extend(b"PK\x05\x06");
        out.extend([0u8; 4]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    #[test]
    fn platforms_are_named_the_way_wakatime_cli_names_its_builds() {
        assert_eq!(platform_of("linux", "x86_64"), Some("linux-amd64"));
        assert_eq!(platform_of("macos", "aarch64"), Some("darwin-arm64"));
        assert_eq!(platform_of("windows", "x86_64"), Some("windows-amd64"));
        assert_eq!(platform_of("freebsd", "x86_64"), None);
        assert_eq!(file_name("windows-arm64"), "wakatime-cli-windows-arm64.exe");
        assert_eq!(file_name("linux-amd64"), "wakatime-cli-linux-amd64");
        for (os, arch) in [("linux", "x86_64"), ("linux", "aarch64"), ("macos", "x86_64"), ("macos", "aarch64"), ("windows", "x86_64"), ("windows", "aarch64")] {
            let s = sum(platform_of(os, arch).unwrap());
            assert!(s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()), "every platform Kairos runs on has a checksum");
        }
    }

    #[test]
    fn the_cli_plugins_installed_comes_before_one_on_the_path() {
        let d = Dir::new("find");
        let (res, bin) = (d.0.join("res"), d.0.join("bin"));
        fs::create_dir_all(&res).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(find_in(&res, Some(&path), "linux-amd64"), None);
        fs::write(bin.join("wakatime-cli"), "").unwrap();
        assert_eq!(find_in(&res, Some(&path), "linux-amd64"), Some(bin.join("wakatime-cli")));
        fs::write(res.join("wakatime-cli"), "").unwrap();
        assert_eq!(find_in(&res, Some(&path), "linux-amd64"), Some(res.join("wakatime-cli")));
        fs::write(res.join("wakatime-cli-linux-amd64"), "").unwrap();
        assert_eq!(find_in(&res, Some(&path), "linux-amd64"), Some(res.join("wakatime-cli-linux-amd64")));
        assert_eq!(find_in(&res, None, "windows-amd64"), None, "windows looks for .exe names");
    }

    #[test]
    fn only_the_expected_zip_is_unpacked() {
        let d = Dir::new("place");
        let program = b"\x7fELF pretend wakatime-cli ".repeat(500);
        let zip = zip_of(&[("README.md", b"hello", false), ("wakatime-cli-linux-amd64", &program, true)]);
        let good = sha256_hex(&zip);

        assert!(place(&zip, &"0".repeat(64), "wakatime-cli-linux-amd64", &d.0).is_err());
        assert!(!d.0.join("wakatime-cli-linux-amd64").exists(), "a zip with another checksum is never unpacked");

        let dest = place(&zip, &good, "wakatime-cli-linux-amd64", &d.0).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), program);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert_eq!(fs::read_dir(&d.0).unwrap().count(), 1, "no temp file left");

        assert_eq!(unzip_one(&zip, "README.md").unwrap(), b"hello");
        assert!(unzip_one(&zip, "wakatime-cli-darwin-arm64").is_err());
        let mut broken = zip.clone();
        let data = zip.windows(24).position(|w| w == b"wakatime-cli-linux-amd64").unwrap() + 24;
        broken[data + 2] ^= 0xff;
        assert!(unzip_one(&broken, "wakatime-cli-linux-amd64").is_err(), "a damaged entry fails its crc instead of being written");
        assert!(unzip_one(b"not a zip at all, just some bytes", "x").is_err());
    }

    #[test]
    #[ignore = "downloads every official wakatime-cli zip"]
    fn the_pinned_checksums_match_the_official_zips() {
        let dir = std::env::temp_dir().join(format!("kairos-wakatime-cli-real-{}", std::process::id()));
        for (platform, sum) in SUMS {
            let zip = download(platform).unwrap();
            let bin = place(&zip, sum, &file_name(platform), &dir).unwrap();
            assert!(fs::metadata(&bin).unwrap().len() > 1 << 20, "{platform}");
        }
        if let Some(p) = platform() {
            let out = std::process::Command::new(dir.join(file_name(p))).arg("--version").output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), VERSION);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
