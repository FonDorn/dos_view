//! Test-only scaffolding: a scratch directory and a file to point the viewer
//! at. A tiny stand-in for the tempfile crate, so the project has no dev
//! dependencies.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Tests run in parallel inside one process, and the clock is not fine enough
/// to tell two of them apart: without this counter two directories now and
/// then got the same name, and the first one dropped deleted the other one's
/// files. That looked like random unrelated test failures.
static SEQ: AtomicU64 = AtomicU64::new(0);

pub struct Dir(PathBuf);

impl Dir {
    pub fn new() -> Self {
        let mut p = std::env::temp_dir();
        let uniq = format!(
            "dosview-test-{}-{}-{:?}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        p.push(uniq);
        // create_dir, not create_dir_all: a name that somehow still collided
        // should fail here and now rather than have two tests share a
        // directory and delete each other's files.
        std::fs::create_dir(&p).unwrap();
        Dir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Write `bytes` to `name` inside the directory and return the path.
    pub fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
