//! A throwaway temp directory for tests that need real files.
//!
//! Kept deliberately small and dependency-free: a unique path per test, a
//! `write` that creates parents, and removal on drop. Every test that uses it
//! works entirely inside the folder it creates and touches nothing else on
//! disk.

#![cfg(test)]

use super::new_id;
use std::path::{Path, PathBuf};

/// A private temp directory, removed when the value is dropped.
pub struct Sandbox(PathBuf);

impl Sandbox {
    /// A fresh, empty directory named after the calling test.
    pub fn new(tag: &str) -> Sandbox {
        let d = std::env::temp_dir().join(format!(
            "openleash-review-{}-{}-{}",
            tag,
            std::process::id(),
            new_id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("sandbox directory");
        Sandbox(d)
    }

    /// Write `body` to `rel` under the sandbox, creating parent directories.
    pub fn write(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("parent directory");
        }
        std::fs::write(&p, body).expect("sandbox write");
        p
    }

    /// The sandbox root.
    pub fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
