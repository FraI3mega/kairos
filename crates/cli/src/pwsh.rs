//! PowerShell never tells Windows when you cd, so kairosd, which asks Windows for a shell's folder, sees the one a
//! PowerShell tab started in. A marked block in their PowerShell profiles fixes that, and `kairos uninstall` takes it out.
#![cfg_attr(not(windows), allow(dead_code))]

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

const BEGIN: &str = "# >>> kairos: keep the process folder in step with cd >>>";
const END: &str = "# <<< kairos <<<";
const BODY: &str = include_str!("pwsh_profile.ps1");

enum Encoding {
    // Anything without a UTF-16 BOM is kept byte for byte (UTF-8 with or without BOM, or the ANSI code page): every
    // byte maps to one char and back, and the block is ASCII.
    Bytes,
    Utf16(bool),
}

fn decode(bytes: &[u8]) -> (Encoding, String) {
    let utf16 = |big: bool| {
        let units = bytes[2..].chunks_exact(2).map(|c| if big { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) });
        (Encoding::Utf16(big), char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect())
    };
    match bytes {
        [0xff, 0xfe, ..] => utf16(false),
        [0xfe, 0xff, ..] => utf16(true),
        _ => (Encoding::Bytes, bytes.iter().map(|&b| char::from(b)).collect()),
    }
}

fn encode(enc: &Encoding, text: &str) -> Vec<u8> {
    match enc {
        Encoding::Bytes => text.chars().map(|c| c as u8).collect(),
        Encoding::Utf16(big) => {
            let bom: &[u8] = if *big { &[0xfe, 0xff] } else { &[0xff, 0xfe] };
            let units = text.encode_utf16().flat_map(|u| if *big { u.to_be_bytes() } else { u.to_le_bytes() });
            bom.iter().copied().chain(units).collect()
        }
    }
}

fn block(nl: &str) -> String {
    let mut s = format!("{BEGIN}{nl}");
    for line in BODY.lines() {
        s.push_str(line);
        s.push_str(nl);
    }
    s.push_str(END);
    s.push_str(nl);
    s
}

// Where the block is, its line ending included. An opening marker without its closing one is left alone.
fn find(text: &str) -> Result<Option<(usize, usize)>> {
    let Some(start) = text.find(BEGIN) else { return Ok(None) };
    let Some(end) = text[start..].find(END).map(|i| start + i + END.len()) else {
        bail!("its Kairos lines have no end marker, so they were left as they are")
    };
    let rest = &text[end..];
    let nl = if rest.starts_with("\r\n") { 2 } else { usize::from(rest.starts_with('\n')) };
    Ok(Some((start, end + nl)))
}

fn with_block(text: &str) -> Result<String> {
    let nl = if text.contains("\r\n") || !text.contains('\n') { "\r\n" } else { "\n" };
    let mut out = text.to_string();
    match find(text)? {
        Some((start, end)) => out.replace_range(start..end, &block(nl)),
        None => {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(nl);
            }
            out.push_str(&block(nl));
        }
    }
    Ok(out)
}

fn without_block(text: &str) -> Result<Option<String>> {
    Ok(find(text)?.map(|(start, end)| format!("{}{}", &text[..start], &text[end..])))
}

pub fn has(profile: &Path) -> bool {
    fs::read(profile).is_ok_and(|b| decode(&b).1.contains(BEGIN))
}

/// Adds the block, or brings one already there up to date. True when the file changed.
pub fn add(profile: &Path) -> Result<bool> {
    let (enc, text) = match fs::read(profile) {
        Ok(b) => decode(&b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Encoding::Bytes, String::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", profile.display())),
    };
    let new = with_block(&text).with_context(|| profile.display().to_string())?;
    if new == text {
        return Ok(false);
    }
    if let Some(dir) = profile.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    fs::write(profile, encode(&enc, &new)).with_context(|| format!("writing {}", profile.display()))?;
    Ok(true)
}

/// Takes the block out and nothing else; a profile left empty goes. True when there was a block.
pub fn remove(profile: &Path) -> Result<bool> {
    let Ok(bytes) = fs::read(profile) else { return Ok(false) };
    let (enc, text) = decode(&bytes);
    let Some(new) = without_block(&text).with_context(|| profile.display().to_string())? else { return Ok(false) };
    if new.is_empty() {
        fs::remove_file(profile).with_context(|| format!("removing {}", profile.display()))?;
    } else {
        fs::write(profile, encode(&enc, &new)).with_context(|| format!("writing {}", profile.display()))?;
    }
    Ok(true)
}

#[cfg(windows)]
mod os {
    use std::path::PathBuf;

    use super::*;
    use crate::install::output;

    struct Shell {
        profile: PathBuf,
        // A profile only runs under RemoteSigned, Unrestricted or Bypass; under Restricted or AllSigned the block would
        // put an error in every new window.
        policy: String,
    }

    impl Shell {
        fn runs_profiles(&self) -> bool {
            matches!(self.policy.as_str(), "RemoteSigned" | "Unrestricted" | "Bypass")
        }
    }

    fn shells() -> Vec<Shell> {
        ["powershell.exe", "pwsh.exe"]
            .into_iter()
            .filter_map(|exe| {
                let out = output(exe, &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "$PROFILE; Get-ExecutionPolicy"])?;
                let mut lines = out.lines().map(str::trim);
                let profile = PathBuf::from(lines.next().filter(|p| !p.is_empty())?);
                Some(Shell { profile, policy: lines.next().unwrap_or_default().to_string() })
            })
            .collect()
    }

    /// Adds the marked lines to every PowerShell profile that runs. Install says so, `kairos uninstall` takes them out.
    pub fn add_all() {
        for s in shells().iter().filter(|s| s.runs_profiles() || has(&s.profile)) {
            let new = !has(&s.profile);
            match add(&s.profile) {
                Ok(_) if new => println!("Added a few marked lines to {} so Kairos sees where you cd in PowerShell (kairos uninstall takes them out).", s.profile.display()),
                Ok(_) => {}
                Err(e) => println!("Could not update the Kairos lines in {}: {e:#}", s.profile.display()),
            }
        }
    }

    pub fn remove_all() {
        let mut profiles: Vec<PathBuf> = shells().into_iter().map(|s| s.profile).collect();
        if let Some(docs) = dirs::document_dir() {
            for dir in ["WindowsPowerShell", "PowerShell"] {
                profiles.push(docs.join(dir).join("Microsoft.PowerShell_profile.ps1"));
            }
        }
        profiles.sort();
        profiles.dedup();
        for p in profiles {
            match remove(&p) {
                Ok(true) => println!("Took the Kairos lines out of {}.", p.display()),
                Ok(false) => {}
                Err(e) => println!("Could not take the Kairos lines out of {}: {e:#}", p.display()),
            }
        }
    }
}

#[cfg(windows)]
pub use os::{add_all, remove_all};

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kairos-pwsh-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("WindowsPowerShell").join("Microsoft.PowerShell_profile.ps1")
    }

    #[test]
    fn adds_once_keeps_the_rest_and_takes_out_exactly_its_block() {
        let p = temp("crlf");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mine = "oh-my-posh init pwsh | Invoke-Expression\r\nSet-Alias g git";
        fs::write(&p, mine).unwrap();
        assert!(add(&p).unwrap());
        let once = fs::read_to_string(&p).unwrap();
        assert!(once.starts_with("oh-my-posh init pwsh | Invoke-Expression\r\nSet-Alias g git\r\n# >>> kairos"), "{once}");
        assert!(once.ends_with("# <<< kairos <<<\r\n"));
        assert!(!once.replace("\r\n", "").contains('\n'), "line endings follow the file");
        assert!(has(&p));
        assert!(!add(&p).unwrap(), "a second install changes nothing");
        assert_eq!(fs::read_to_string(&p).unwrap(), once);

        fs::write(&p, format!("{once}Import-Module posh-git\r\n")).unwrap();
        assert!(remove(&p).unwrap());
        assert_eq!(fs::read_to_string(&p).unwrap(), format!("{mine}\r\nImport-Module posh-git\r\n"));
        assert!(!remove(&p).unwrap());
        assert!(!has(&p));
        let _ = fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn an_old_block_is_replaced_in_place() {
        let p = temp("old");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("before\n{BEGIN}\nold lines\n{END}\nafter\n")).unwrap();
        assert!(add(&p).unwrap());
        let text = fs::read_to_string(&p).unwrap();
        assert_eq!(text, format!("before\n{}after\n", block("\n")));
        assert_eq!(text.matches(BEGIN).count(), 1);
        let _ = fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_profile_kairos_made_goes_with_its_block() {
        let p = temp("new");
        assert!(add(&p).unwrap());
        assert_eq!(fs::read_to_string(&p).unwrap(), block("\r\n"));
        assert!(fs::read_to_string(&p).unwrap().contains("[Environment]::CurrentDirectory"));
        assert!(remove(&p).unwrap());
        assert!(!p.exists());
        let _ = fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn other_encodings_survive() {
        let p = temp("enc");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let utf16 = |text: &str| -> Vec<u8> { [0xff, 0xfe].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect() };
        let original = utf16("# café\r\n");
        fs::write(&p, &original).unwrap();
        assert!(add(&p).unwrap());
        let added = fs::read(&p).unwrap();
        assert_eq!(added, utf16(&format!("# café\r\n{}", block("\r\n"))));
        assert!(remove(&p).unwrap());
        assert_eq!(fs::read(&p).unwrap(), original);

        for original in [b"\xef\xbb\xbf# caf\xc3\xa9\n".to_vec(), b"# caf\xe9 in ANSI\r\n".to_vec()] {
            fs::write(&p, &original).unwrap();
            assert!(add(&p).unwrap());
            assert!(fs::read(&p).unwrap().starts_with(&original));
            assert!(remove(&p).unwrap());
            assert_eq!(fs::read(&p).unwrap(), original);
        }
        let _ = fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_block_without_its_end_is_left_alone() {
        let p = temp("broken");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let text = format!("{BEGIN}\nsomething the person changed\n");
        fs::write(&p, &text).unwrap();
        assert!(add(&p).is_err());
        assert!(remove(&p).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), text);
        let _ = fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }
}
