//! Shared helpers for the integration tests. Each test works inside its own temporary
//! directory, removed on drop, so no test reads or writes a real home, config, or credential.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mcao_core::home::Env;

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("mcao-{label}-{}-{serial}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create isolated temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Builds an environment from literal or owned pairs.
pub fn env<V: AsRef<str>>(pairs: &[(&str, V)]) -> Env {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.as_ref().to_owned()))
        .collect()
}

pub fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Writes a fixture, creating its parent directories first.
pub fn write(path: &Path, content: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, content).expect("write fixture");
}
