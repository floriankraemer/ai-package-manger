//! Exclusive advisory file locks, so concurrent aip runs do not corrupt shared state.

use crate::{Context, Result};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::Path;

/// Exclusive advisory lock held until dropped.
pub struct FileLock {
    _file: File,
}

/// Locks `path` (created with parent folders if missing). If another process holds it,
/// prints `Waiting for another aip process ({what})...` to stderr once, then blocks.
pub fn acquire(path: &Path, what: &str) -> Result<FileLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .context(|| format!("cannot create lock directory {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .context(|| format!("cannot open lock file {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            eprintln!("Waiting for another aip process ({what})...");
            file.lock()
                .context(|| format!("cannot lock {}", path.display()))?;
        }
        Err(TryLockError::Error(e)) => {
            return Err(crate::Error::new(format!(
                "cannot lock {}: {e}",
                path.display()
            )));
        }
    }
    Ok(FileLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use std::{env, thread};

    #[test]
    fn second_acquire_waits_for_release() {
        let path = env::temp_dir().join(format!("aip-lock-{}/test.lock", std::process::id()));
        let first = acquire(&path, "test").unwrap();
        let other = path.clone();
        let waiter = thread::spawn(move || {
            let _lock = acquire(&other, "test").unwrap();
            Instant::now()
        });
        thread::sleep(Duration::from_millis(300));
        let released = Instant::now();
        drop(first);
        let acquired = waiter.join().unwrap();
        assert!(acquired >= released);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
