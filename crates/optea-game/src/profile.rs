//! Discovery of Siege's per-Ubisoft-account settings folders.
//!
//! Siege stores settings under `Documents\My Games\Rainbow Six - Siege\<uuid>\`,
//! one folder per Ubisoft account that has signed in on this machine. Machines
//! with more than one folder are common, and writing to the wrong one silently
//! does nothing — so the active profile is resolved by recency of
//! `GameSettings.ini` rather than by picking the first directory found.

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const SETTINGS_FILE: &str = "GameSettings.ini";
const MY_GAMES: &str = "My Games";
const SIEGE_DIR: &str = "Rainbow Six - Siege";

#[derive(Debug, thiserror::Error)]
pub enum GameError {
    #[error("could not resolve Documents folder: {0}")]
    Documents(#[from] optea_sys::SysError),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, GameError>;

#[derive(Debug, Clone, Serialize)]
pub struct SiegeProfile {
    /// The folder name, which is the Ubisoft profile UUID.
    pub id: String,
    pub dir: PathBuf,
    pub settings_path: PathBuf,
    /// Last-modified time of `GameSettings.ini`, used to rank profiles.
    #[serde(skip)]
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SiegeProfiles {
    pub root: PathBuf,
    /// Sorted most-recently-modified first.
    pub profiles: Vec<SiegeProfile>,
}

impl SiegeProfiles {
    /// The profile Siege most recently wrote to.
    pub fn active(&self) -> Option<&SiegeProfile> {
        self.profiles.first()
    }

    pub fn is_ambiguous(&self) -> bool {
        self.profiles.len() > 1
    }
}

/// Locate Siege's settings root and every profile inside it.
///
/// Returns `Ok(None)` when Siege has never stored settings on this machine.
pub fn discover() -> Result<Option<SiegeProfiles>> {
    let documents = optea_sys::paths::documents()?;
    let root = documents.join(MY_GAMES).join(SIEGE_DIR);
    if !root.is_dir() {
        return Ok(None);
    }
    Ok(Some(scan_root(&root)?))
}

/// Scan a settings root directly. Separated from [`discover`] so it can be
/// tested against a fixture tree without touching the real Documents folder.
pub fn scan_root(root: &Path) -> Result<SiegeProfiles> {
    let entries = fs::read_dir(root).map_err(|source| GameError::Io {
        path: root.to_path_buf(),
        source,
    })?;

    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let settings_path = dir.join(SETTINGS_FILE);
        if !settings_path.is_file() {
            continue; // a directory without settings is not a usable profile
        }
        let modified = fs::metadata(&settings_path).ok().and_then(|m| m.modified().ok());
        profiles.push(SiegeProfile {
            id: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir,
            settings_path,
            modified,
        });
    }

    // Most recent first; profiles with no timestamp sort last.
    profiles.sort_by(|a, b| match (b.modified, a.modified) {
        (Some(bm), Some(am)) => bm.cmp(&am),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.id.cmp(&b.id),
    });

    Ok(SiegeProfiles {
        root: root.to_path_buf(),
        profiles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    /// Build a fixture tree under the OS temp dir.
    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("optea-test-{name}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn add_profile(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE);
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "[DISPLAY]").unwrap();
        path
    }

    #[test]
    fn picks_most_recently_modified_profile() {
        let root = fixture("recency");
        add_profile(&root, "aaaa-old");
        // Ensure a distinct mtime; filesystem timestamp granularity can be coarse.
        std::thread::sleep(Duration::from_millis(20));
        let newer = add_profile(&root, "bbbb-new");
        let now = SystemTime::now();
        filetime_touch(&newer, now);

        let found = scan_root(&root).unwrap();
        assert_eq!(found.profiles.len(), 2);
        assert!(found.is_ambiguous());
        assert_eq!(
            found.active().unwrap().id,
            "bbbb-new",
            "active profile must be the most recently written one"
        );
    }

    /// Set mtime by rewriting the file, which is enough to bump it forward.
    fn filetime_touch(path: &Path, _when: SystemTime) {
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "; touched").unwrap();
        f.sync_all().unwrap();
    }

    #[test]
    fn ignores_directories_without_settings() {
        let root = fixture("nosettings");
        add_profile(&root, "real-profile");
        fs::create_dir_all(root.join("empty-dir")).unwrap();

        let found = scan_root(&root).unwrap();
        assert_eq!(found.profiles.len(), 1);
        assert_eq!(found.active().unwrap().id, "real-profile");
        assert!(!found.is_ambiguous());
    }

    #[test]
    fn empty_root_yields_no_profiles() {
        let root = fixture("empty");
        let found = scan_root(&root).unwrap();
        assert!(found.profiles.is_empty());
        assert!(found.active().is_none());
    }

    #[test]
    fn discovery_on_this_machine_does_not_error() {
        // Machines without Siege return Ok(None); machines with it return a root.
        let found = discover().unwrap();
        if let Some(p) = found {
            assert!(p.root.is_dir());
            for prof in &p.profiles {
                assert!(prof.settings_path.is_file());
            }
        }
    }
}
