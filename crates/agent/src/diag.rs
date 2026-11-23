use std::fmt::Arguments;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::Local;

const MAX_BYTES: u64 = 1024 * 1024;

struct Rotating {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    max: u64,
}

impl Rotating {
    fn open(path: PathBuf, max: u64) -> std::io::Result<Rotating> {
        let (file, size) = Rotating::start(&path, max)?;
        Ok(Rotating { path, file: Some(file), size, max })
    }

    fn start(path: &Path, max: u64) -> std::io::Result<(File, u64)> {
        if fs::metadata(path).map_or(false, |m| m.len() > max) {
            let _ = fs::rename(path, path.with_extension("log.1"));
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let size = file.metadata()?.len();
        Ok((file, size))
    }

    fn line(&mut self, text: &str) {
        if self.size > self.max {
            self.file = None;
            if let Ok((file, size)) = Rotating::start(&self.path, self.max) {
                self.file = Some(file);
                self.size = size;
            }
        }
        let Some(file) = self.file.as_mut() else { return };
        let line = format!("{} {text}\n", Local::now().format("%Y-%m-%d %H:%M:%S"));
        if file.write_all(line.as_bytes()).is_ok() {
            self.size += line.len() as u64;
        }
    }
}

static FILE: OnceLock<Mutex<Rotating>> = OnceLock::new();

pub fn init(dir: &Path) {
    if cfg!(windows) {
        let _ = fs::create_dir_all(dir);
        if let Ok(r) = Rotating::open(dir.join("kairosd.log"), MAX_BYTES) {
            let _ = FILE.set(Mutex::new(r));
        }
    }
}

pub fn write(args: Arguments) {
    if cfg!(windows) {
        if let Some(f) = FILE.get() {
            if let Ok(mut f) = f.lock() {
                f.line(&args.to_string());
            }
        }
    } else {
        eprintln!("{args}");
    }
}

macro_rules! diag {
    ($($t:tt)*) => {
        $crate::diag::write(format_args!($($t)*))
    };
}
pub(crate) use diag;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_above_the_limit() {
        let dir = std::env::temp_dir().join(format!("kairos-diag-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kairosd.log");
        let mut r = Rotating::open(path.clone(), 100).unwrap();
        for i in 0..10 {
            r.line(&format!("kairosd: line number {i}"));
        }
        let old = fs::read_to_string(dir.join("kairosd.log.1")).unwrap();
        let new = fs::read_to_string(&path).unwrap();
        assert!(old.contains("line number"));
        assert!(new.contains("line number 9"));
        assert!(new.len() as u64 <= 100 + 60);
        let _ = fs::remove_dir_all(&dir);
    }
}
