
use std::path::Path;

use anyhow::{bail, Result};
use kairos_core::{exclude, paths};

const USAGE: &str = "usage: kairos exclude [PATH]\n       kairos exclude --list\n       kairos exclude --remove PATH";

pub fn command(args: &[String]) -> Result<()> {
    println!("{}", run(&paths::data_dir(), args)?);
    Ok(())
}

fn run(data: &Path, args: &[String]) -> Result<String> {
    match args {
        [] => add(data, Path::new(".")),
        [flag] if flag == "--list" => list(data),
        [flag, path] if flag == "--remove" => remove(data, Path::new(path)),
        [path] if !path.starts_with('-') => add(data, Path::new(path)),
        _ => bail!("{USAGE}"),
    }
}

fn add(data: &Path, path: &Path) -> Result<String> {
    let root = exclude::add(data, path)?;
    Ok(format!(
        "Kairos won't record anything from {}: no prompts, edits, commits, file names or time spent there. \
         What it recorded before stays.",
        root.display()
    ))
}

fn list(data: &Path) -> Result<String> {
    let ex = exclude::load(data)?;
    if ex.roots.is_empty() {
        return Ok("No folders are excluded. Run kairos exclude in a folder Kairos should never record.".into());
    }
    let lines: Vec<String> = ex.roots.iter().map(|r| format!("  {}", r.display())).collect();
    Ok(format!("Kairos does not record anything from these folders:\n{}", lines.join("\n")))
}

fn remove(data: &Path, path: &Path) -> Result<String> {
    Ok(if exclude::remove(data, path)? {
        format!("Kairos records {} again. Time spent there while it was excluded stays unrecorded.", path.display())
    } else {
        format!("{} is not on the excluded list. kairos exclude --list shows what is.", path.display())
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_list_and_remove() {
        let data = std::env::temp_dir().join(format!("kairos-cli-exclude-{}", std::process::id()));
        let _ = fs::remove_dir_all(&data);
        let (secret, other) = (data.join("work").join("secret"), data.join("work").join("secrets"));
        let (s, o) = (secret.to_string_lossy().into_owned(), other.to_string_lossy().into_owned());
        let run = |a: &[&str]| run(&data, &args(a));

        assert!(run(&["--list"]).unwrap().starts_with("No folders are excluded"));
        let said = run(&[s.as_str()]).unwrap();
        assert!(said.starts_with(&format!("Kairos won't record anything from {s}:")), "{said}");
        assert!(said.contains("What it recorded before stays."));
        run(&[o.as_str()]).unwrap();
        let listed = run(&["--list"]).unwrap();
        assert!(listed.contains(&format!("  {s}\n  {o}")), "{listed}");
        assert!(run(&["--remove", s.as_str()]).unwrap().starts_with(&format!("Kairos records {s} again.")));
        assert!(run(&["--remove", s.as_str()]).unwrap().contains("is not on the excluded list"));
        let listed = run(&["--list"]).unwrap();
        assert!(!listed.contains(&format!("  {s}\n")) && listed.contains(&o), "{listed}");

        run(&[]).unwrap();
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert!(exclude::load(&data).unwrap().covers(&here), "no path means the current folder");

        let bad: [&[&str]; 5] = [&["--remove"], &["--bogus"], &["--list", "x"], &["a", "b"], &["--remove", "a", "b"]];
        for bad in bad {
            assert!(run(bad).unwrap_err().to_string().starts_with("usage: kairos exclude"), "{bad:?}");
        }
        let _ = fs::remove_dir_all(&data);
    }
}
