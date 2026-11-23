use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

pub struct Instance(#[allow(dead_code)] File);

pub fn acquire(dir: &Path) -> Result<Option<Instance>> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("kairosd.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {
            let _ = fs::write(dir.join("kairosd.pid"), std::process::id().to_string());
            Ok(Some(Instance(file)))
        }
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("locking {}", path.display())),
    }
}

pub fn acquire_within(dir: &Path, wait: Duration) -> Result<Option<Instance>> {
    let deadline = Instant::now() + wait;
    loop {
        if let Some(i) = acquire(dir)? {
            return Ok(Some(i));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn second_holder_is_refused_until_the_first_drops() {
        let dir = std::env::temp_dir().join(format!("kairos-instance-{}", std::process::id()));
        let first = super::acquire(&dir).unwrap();
        assert!(first.is_some());
        assert!(super::acquire(&dir).unwrap().is_none());
        drop(first);
        // A child that another test spawns in this process can hold a copy of the descriptor until it execs.
        assert!(super::acquire_within(&dir, std::time::Duration::from_secs(5)).unwrap().is_some());
        assert_eq!(std::fs::read_to_string(dir.join("kairosd.pid")).unwrap(), std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod wait_tests {
    use std::time::Duration;

    #[test]
    fn waits_for_the_holder_to_let_go() {
        let dir = std::env::temp_dir().join(format!("kairos-instance-wait-{}", std::process::id()));
        let first = super::acquire(&dir).unwrap();
        assert!(super::acquire_within(&dir, Duration::from_millis(300)).unwrap().is_none());
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(first);
        });
        assert!(super::acquire_within(&dir, Duration::from_secs(5)).unwrap().is_some());
        t.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
