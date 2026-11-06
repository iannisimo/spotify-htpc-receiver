//! One UI at a time: the UI holds a lock file while it runs, and the watcher checks it before
//! launching another.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

const LOCK_NAME: &str = "htpc-nowplaying.lock";

/// `$XDG_RUNTIME_DIR/htpc-nowplaying.lock`, or the user's cache directory without one.
pub fn default_lock_path() -> PathBuf {
    lock_path_from(
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        std::env::home_dir(),
    )
}

fn lock_path_from(runtime_dir: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    runtime_dir
        .filter(|d| d.is_absolute())
        .or_else(|| home.map(|h| h.join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join(LOCK_NAME)
}

/// Held while the UI runs; released when dropped (or when the process ends, however it ends).
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// Takes the lock. `Ok(None)` means another instance holds it.
pub fn acquire(path: &Path) -> io::Result<Option<InstanceLock>> {
    let file = open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Whether some process holds the lock right now.
pub fn is_held(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    match acquire(path) {
        Ok(lock) => lock.is_none(),
        Err(e) => {
            log::warn!("cannot check {}: {e}", path.display());
            false
        }
    }
}

fn open(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("htpc-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join(LOCK_NAME)
    }

    #[test]
    fn second_acquire_fails_while_held_and_works_after_release() {
        let path = temp_lock("twice");
        let first = acquire(&path).unwrap();
        assert!(first.is_some());
        assert!(is_held(&path));
        assert!(acquire(&path).unwrap().is_none());
        drop(first);
        assert!(!is_held(&path));
        assert!(acquire(&path).unwrap().is_some());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_lock_file_is_not_held_and_not_created_by_checking() {
        let path = temp_lock("missing");
        assert!(!is_held(&path));
        assert!(!path.exists());
    }

    #[test]
    fn path_prefers_runtime_dir() {
        let home = Some(PathBuf::from("/home/me"));
        assert_eq!(
            lock_path_from(Some("/run/user/1000".into()), home.clone()),
            PathBuf::from("/run/user/1000/htpc-nowplaying.lock")
        );
        assert_eq!(
            lock_path_from(None, home.clone()),
            PathBuf::from("/home/me/.cache/htpc-nowplaying.lock")
        );
        assert_eq!(
            lock_path_from(Some("relative".into()), home),
            PathBuf::from("/home/me/.cache/htpc-nowplaying.lock")
        );
    }
}
