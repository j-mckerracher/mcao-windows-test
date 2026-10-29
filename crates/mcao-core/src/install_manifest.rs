//! Pure data model for `mcao` installation manifests on Windows (DPES-892).
//!
//! An install manifest records which files an installation placed so an uninstallation
//! removes exactly those files. This module does not access the filesystem, registry,
//! or Windows API.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The current schema version for install manifests.
pub const MANIFEST_VERSION: u32 = 1;

/// A record of a single file installed on the system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledFile {
    /// Relative path of the file from the installation root, normalised to `/`.
    pub relative_path: String,
    /// Size of the file in bytes.
    pub size: u64,
}

/// Manifest recording the files and state of an installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallManifest {
    /// Schema version of this manifest.
    pub version: u32,
    /// Target directory where files were installed.
    pub install_dir: String,
    /// List of installed files in insertion order.
    pub files: Vec<InstalledFile>,
    /// Whether an entry was added to the system or user `PATH`.
    pub path_entry_added: bool,
}

/// Errors that can occur when manipulating or deserializing install manifests.
#[derive(Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// The manifest version is not supported.
    UnsupportedVersion(u32),
    /// A file path violates safety rules (e.g. traversal, absolute, or invalid characters).
    UnsafePath(String),
    /// A file path duplicates an existing entry (ASCII case-insensitive).
    Duplicate(String),
    /// The manifest JSON could not be parsed.
    Parse(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported manifest version: {version}")
            }
            Self::UnsafePath(path) => write!(f, "unsafe path: {path}"),
            Self::Duplicate(path) => write!(f, "duplicate path: {path}"),
            Self::Parse(message) => write!(f, "manifest parse error: {message}"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl InstallManifest {
    /// Creates a new manifest for the given installation directory.
    pub fn new(install_dir: &str) -> Self {
        Self {
            version: MANIFEST_VERSION,
            install_dir: install_dir.to_owned(),
            files: Vec::new(),
            path_entry_added: false,
        }
    }

    /// Adds an installed file after validating and normalising its relative path.
    ///
    /// The path must be non-empty, not absolute (no leading `/` or `\`, no drive prefix
    /// such as `C:`), contain no `..` segments, no empty segments from `//` or `\\`,
    /// no NUL bytes, and no `:` characters. Backslashes are normalised to `/`.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::UnsafePath`] if the path violates safety rules.
    /// Returns [`ManifestError::Duplicate`] if the path matches an already-added file
    /// (ASCII case-insensitive).
    pub fn add_file(&mut self, relative_path: &str, size: u64) -> Result<(), ManifestError> {
        if relative_path.is_empty()
            || relative_path.starts_with('/')
            || relative_path.starts_with('\\')
            || relative_path.contains(':')
            || relative_path.contains('\0')
        {
            return Err(ManifestError::UnsafePath(relative_path.to_owned()));
        }

        let normalized = relative_path.replace('\\', "/");

        for segment in normalized.split('/') {
            if segment.is_empty() || segment == ".." {
                return Err(ManifestError::UnsafePath(relative_path.to_owned()));
            }
        }

        if self
            .files
            .iter()
            .any(|f| f.relative_path.eq_ignore_ascii_case(&normalized))
        {
            return Err(ManifestError::Duplicate(relative_path.to_owned()));
        }

        self.files.push(InstalledFile {
            relative_path: normalized,
            size,
        });

        Ok(())
    }

    /// Serializes this manifest to a JSON string.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Parses an install manifest from JSON text.
    ///
    /// Rejects unsupported manifest versions, re-validates all file paths under the
    /// same rules as [`add_file`], rejects duplicates, and maps parse errors to
    /// [`ManifestError::Parse`]. Parse error messages will not echo file contents
    /// beyond 80 characters.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::UnsupportedVersion`] if the version is not [`MANIFEST_VERSION`].
    /// Returns [`ManifestError::UnsafePath`] if any file path is invalid.
    /// Returns [`ManifestError::Duplicate`] if duplicate file paths exist.
    /// Returns [`ManifestError::Parse`] if the JSON is malformed.
    pub fn from_json(text: &str) -> Result<Self, ManifestError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            version: Option<u32>,
        }

        if let Ok(probe) = serde_json::from_str::<VersionProbe>(text) {
            match probe.version {
                Some(v) if v != MANIFEST_VERSION => {
                    return Err(ManifestError::UnsupportedVersion(v));
                }
                _ => {}
            }
        }

        let raw: Self = serde_json::from_str(text).map_err(|e| {
            let msg: String = e.to_string().chars().take(80).collect();
            ManifestError::Parse(msg)
        })?;

        if raw.version != MANIFEST_VERSION {
            return Err(ManifestError::UnsupportedVersion(raw.version));
        }

        let mut manifest = Self {
            version: raw.version,
            install_dir: raw.install_dir,
            files: Vec::with_capacity(raw.files.len()),
            path_entry_added: raw.path_entry_added,
        };

        for file in raw.files {
            manifest.add_file(&file.relative_path, file.size)?;
        }

        Ok(manifest)
    }

    /// Returns file paths sorted by depth descending, with ties broken in reverse insertion order.
    pub fn removal_order(&self) -> Vec<String> {
        let mut items: Vec<(usize, usize, &str)> = self
            .files
            .iter()
            .enumerate()
            .map(|(idx, f)| {
                (
                    f.relative_path.split('/').count(),
                    idx,
                    f.relative_path.as_str(),
                )
            })
            .collect();
        items.sort_by_key(|&(depth, idx, _)| std::cmp::Reverse((depth, idx)));
        items
            .into_iter()
            .map(|(_, _, path)| path.to_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_round_trip() {
        let mut manifest = InstallManifest::new(r"C:\Program Files\mcao");
        manifest.add_file("bin/mcao.exe", 1024).unwrap();
        manifest.add_file("lib/helper.dll", 2048).unwrap();
        manifest.path_entry_added = true;

        let json = manifest.to_json();
        let restored = InstallManifest::from_json(&json).unwrap();
        assert_eq!(manifest, restored);
        assert_eq!(restored.files.len(), 2);
        assert_eq!(restored.files[0].relative_path, "bin/mcao.exe");
        assert_eq!(restored.files[0].size, 1024);
        assert_eq!(restored.files[1].relative_path, "lib/helper.dll");
        assert_eq!(restored.files[1].size, 2048);
        assert!(restored.path_entry_added);
    }

    #[test]
    fn test_unsafe_paths() {
        let mut manifest = InstallManifest::new("C:/test");

        // empty
        assert_eq!(
            manifest.add_file("", 10),
            Err(ManifestError::UnsafePath("".to_string()))
        );

        // /abs
        assert_eq!(
            manifest.add_file("/abs", 10),
            Err(ManifestError::UnsafePath("/abs".to_string()))
        );

        // \\server
        assert_eq!(
            manifest.add_file(r"\\server", 10),
            Err(ManifestError::UnsafePath(r"\\server".to_string()))
        );

        // C:\x
        assert_eq!(
            manifest.add_file(r"C:\x", 10),
            Err(ManifestError::UnsafePath(r"C:\x".to_string()))
        );

        // a/../b
        assert_eq!(
            manifest.add_file("a/../b", 10),
            Err(ManifestError::UnsafePath("a/../b".to_string()))
        );

        // a//b
        assert_eq!(
            manifest.add_file("a//b", 10),
            Err(ManifestError::UnsafePath("a//b".to_string()))
        );

        // a:stream
        assert_eq!(
            manifest.add_file("a:stream", 10),
            Err(ManifestError::UnsafePath("a:stream".to_string()))
        );

        // NUL
        assert_eq!(
            manifest.add_file("a\0b", 10),
            Err(ManifestError::UnsafePath("a\0b".to_string()))
        );
    }

    #[test]
    fn test_backslash_normalised() {
        let mut manifest = InstallManifest::new("C:/test");
        manifest.add_file(r"bin\tools\mcao.exe", 100).unwrap();
        assert_eq!(manifest.files[0].relative_path, "bin/tools/mcao.exe");
        assert_eq!(manifest.files[0].size, 100);
    }

    #[test]
    fn test_duplicate_case_insensitive() {
        let mut manifest = InstallManifest::new("C:/test");
        manifest.add_file("bin/mcao.exe", 100).unwrap();

        // ASCII case-insensitive duplicate with same slashes
        assert_eq!(
            manifest.add_file("BIN/MCAO.EXE", 200),
            Err(ManifestError::Duplicate("BIN/MCAO.EXE".to_string()))
        );

        // Duplicate with backslashes
        assert_eq!(
            manifest.add_file(r"bin\mcao.exe", 300),
            Err(ManifestError::Duplicate(r"bin\mcao.exe".to_string()))
        );

        // Mixed case with backslashes
        assert_eq!(
            manifest.add_file(r"Bin\Mcao.Exe", 400),
            Err(ManifestError::Duplicate(r"Bin\Mcao.Exe".to_string()))
        );
    }

    #[test]
    fn test_from_json_version_2() {
        let json = r#"{"version":2,"install_dir":"C:/test","files":[],"path_entry_added":false}"#;
        assert_eq!(
            InstallManifest::from_json(json),
            Err(ManifestError::UnsupportedVersion(2))
        );

        let json_minimal = r#"{"version":2}"#;
        assert_eq!(
            InstallManifest::from_json(json_minimal),
            Err(ManifestError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn test_from_json_with_dot_dot_path() {
        let json = r#"{
            "version": 1,
            "install_dir": "C:/test",
            "files": [
                {"relative_path": "a/../b", "size": 42}
            ],
            "path_entry_added": false
        }"#;
        assert_eq!(
            InstallManifest::from_json(json),
            Err(ManifestError::UnsafePath("a/../b".to_string()))
        );
    }

    #[test]
    fn test_malformed_json() {
        let err = InstallManifest::from_json("invalid json").unwrap_err();
        match err {
            ManifestError::Parse(msg) => {
                assert!(!msg.is_empty());
                assert!(msg.chars().count() <= 80);
            }
            other => panic!("expected Parse error, got {other:?}"),
        }

        let long_bad_json = format!("{{\"version\": 1, \"install_dir\": \"{}\"", "x".repeat(200));
        let err_long = InstallManifest::from_json(&long_bad_json).unwrap_err();
        match err_long {
            ManifestError::Parse(msg) => {
                assert!(msg.chars().count() <= 80);
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn test_removal_order() {
        let mut manifest = InstallManifest::new("C:/test");
        manifest.add_file("a/b/c.txt", 1).unwrap();
        manifest.add_file("d/e.txt", 2).unwrap();
        manifest.add_file("f.txt", 3).unwrap();
        manifest.add_file("a/b/g.txt", 4).unwrap();
        manifest.add_file("h/i.txt", 5).unwrap();

        assert_eq!(
            manifest.removal_order(),
            vec![
                "a/b/g.txt".to_string(),
                "a/b/c.txt".to_string(),
                "h/i.txt".to_string(),
                "d/e.txt".to_string(),
                "f.txt".to_string(),
            ]
        );
    }

    #[test]
    fn test_empty_manifest_round_trip() {
        let manifest = InstallManifest::new("C:/empty");
        let json = manifest.to_json();
        let restored = InstallManifest::from_json(&json).unwrap();
        assert_eq!(manifest, restored);
        assert_eq!(restored.files.len(), 0);
        assert_eq!(restored.removal_order(), Vec::<String>::new());
    }

    #[test]
    fn test_from_json_duplicate() {
        let json = r#"{
            "version": 1,
            "install_dir": "C:/test",
            "files": [
                {"relative_path": "bin/mcao.exe", "size": 10},
                {"relative_path": "BIN/MCAO.EXE", "size": 20}
            ],
            "path_entry_added": false
        }"#;
        assert_eq!(
            InstallManifest::from_json(json),
            Err(ManifestError::Duplicate("BIN/MCAO.EXE".to_string()))
        );
    }

    #[test]
    fn test_error_display() {
        assert_eq!(
            ManifestError::UnsupportedVersion(2).to_string(),
            "unsupported manifest version: 2"
        );
        assert_eq!(
            ManifestError::UnsafePath("..".to_string()).to_string(),
            "unsafe path: .."
        );
        assert_eq!(
            ManifestError::Duplicate("foo".to_string()).to_string(),
            "duplicate path: foo"
        );
        assert_eq!(
            ManifestError::Parse("bad syntax".to_string()).to_string(),
            "manifest parse error: bad syntax"
        );
    }
}
