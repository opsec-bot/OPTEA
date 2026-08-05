//! Backup-enforced file editing.
//!
//! `GameSettings.ini` has no "absent versus zero" fallback like the registry
//! does — if it is lost or mangled, the user's entire configuration is gone,
//! including keybinds and sensitivity they may have tuned over years. So the
//! rule here is stronger than "remember to take a backup":
//!
//! **[`GuardedFile::edit`] is the only way to write, and it cannot run the
//! caller's transform until a backup has been written, re-read, and verified by
//! hash.** Forgetting to back up is not a mistake this API permits.
//!
//! Layers of protection, each independently tested:
//!
//! 1. A **pristine** copy is taken the first time OPTEA ever touches the file
//!    and is never overwritten thereafter — the always-available way back to
//!    the original, however many edits happen later.
//! 2. Every edit additionally takes a **timestamped** backup, so a bad edit
//!    does not consume the only good copy.
//! 3. Backups are verified by reading them back and comparing SHA-256 against
//!    the source. A backup that cannot be proven correct fails the edit.
//! 4. Writes are **atomic** (temp file + rename), so a crash mid-write cannot
//!    truncate the original.
//! 5. If the write fails, the backup is restored automatically.
//! 6. Edits are refused while the game is running, because Siege rewrites this
//!    file on exit and would silently discard the change.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Rolling backups kept per profile, beyond the pristine copy.
const HISTORY_LIMIT: usize = 30;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("{path} does not exist")]
    Missing { path: PathBuf },

    #[error("{path} is read-only; clear the read-only attribute to edit it")]
    ReadOnly { path: PathBuf },

    #[error(
        "the game is running (pid {pid}). Siege rewrites GameSettings.ini when it exits, so any \
         change made now would be silently discarded. Close the game first."
    )]
    GameRunning { pid: u32 },

    #[error(
        "backup verification FAILED for {path}: wrote {written} bytes hashing {got}, expected \
         {expected}. The original file has NOT been modified."
    )]
    VerifyFailed {
        path: PathBuf,
        written: u64,
        got: String,
        expected: String,
    },

    #[error("{path} is not valid UTF-8, so OPTEA will not risk rewriting it")]
    NotUtf8 { path: PathBuf },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "write failed and the original was restored from {backup}: {source}"
    )]
    WriteRolledBack {
        backup: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("edit transform failed: {0}")]
    Transform(String),
}

type Result<T> = std::result::Result<T, BackupError>;

fn io(path: &Path) -> impl Fn(std::io::Error) -> BackupError + '_ {
    move |source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Metadata written beside every backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupMeta {
    pub id: String,
    pub source: String,
    pub sha256: String,
    pub size: u64,
    pub taken_at: String,
    /// True for the never-overwritten original.
    pub pristine: bool,
}

#[derive(Debug, Clone)]
pub struct Backup {
    pub meta: BackupMeta,
    pub data_path: PathBuf,
}

impl Backup {
    /// Re-read the backup and confirm it still matches its recorded hash.
    pub fn verify(&self) -> Result<()> {
        let bytes = std::fs::read(&self.data_path).map_err(io(&self.data_path))?;
        let got = sha256_hex(&bytes);
        if got != self.meta.sha256 {
            return Err(BackupError::VerifyFailed {
                path: self.data_path.clone(),
                written: bytes.len() as u64,
                got,
                expected: self.meta.sha256.clone(),
            });
        }
        Ok(())
    }
}

/// Where backups for one profile live.
pub struct BackupStore {
    dir: PathBuf,
}

impl BackupStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        BackupStore { dir: dir.into() }
    }

    /// `%ProgramData%\OPTEA\backups\<profile-id>`
    pub fn for_profile(profile_id: &str) -> std::result::Result<Self, optea_sys::SysError> {
        Ok(BackupStore::new(
            optea_sys::paths::optea_data_dir()?
                .join("backups")
                .join(profile_id),
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn pristine_data(&self) -> PathBuf {
        self.dir.join("pristine.ini")
    }

    fn pristine_meta(&self) -> PathBuf {
        self.dir.join("pristine.json")
    }

    /// The original file as OPTEA first found it, if one has been taken.
    pub fn pristine(&self) -> Option<Backup> {
        let meta: BackupMeta =
            serde_json::from_str(&std::fs::read_to_string(self.pristine_meta()).ok()?).ok()?;
        Some(Backup {
            meta,
            data_path: self.pristine_data(),
        })
    }

    /// Take the pristine copy if it does not exist yet. Never overwrites.
    pub fn ensure_pristine(&self, source: &Path) -> Result<Backup> {
        if let Some(existing) = self.pristine() {
            // Prove the stored original is still intact rather than assuming it.
            existing.verify()?;
            return Ok(existing);
        }
        self.write_backup(source, "pristine", true)
    }

    /// Take a timestamped backup.
    pub fn take(&self, source: &Path) -> Result<Backup> {
        let backup = self.write_backup(source, &timestamp_id(), false)?;
        self.prune()?;
        Ok(backup)
    }

    /// Copy `source` into the store and verify it landed byte-for-byte.
    fn write_backup(&self, source: &Path, id: &str, pristine: bool) -> Result<Backup> {
        std::fs::create_dir_all(&self.dir).map_err(io(&self.dir))?;

        let bytes = std::fs::read(source).map_err(io(source))?;
        let expected = sha256_hex(&bytes);

        let data_path = self.dir.join(format!("{id}.ini"));
        write_atomic(&data_path, &bytes)?;

        // Read back from disk. A hash of the in-memory buffer would only prove
        // we can hash; this proves the bytes actually survived the round trip.
        let readback = std::fs::read(&data_path).map_err(io(&data_path))?;
        let got = sha256_hex(&readback);
        if got != expected {
            return Err(BackupError::VerifyFailed {
                path: data_path,
                written: readback.len() as u64,
                got,
                expected,
            });
        }

        let meta = BackupMeta {
            id: id.to_string(),
            source: source.display().to_string(),
            sha256: expected,
            size: bytes.len() as u64,
            taken_at: timestamp_id(),
            pristine,
        };
        let meta_path = self.dir.join(format!("{id}.json"));
        write_atomic(
            &meta_path,
            serde_json::to_string_pretty(&meta)
                .map_err(|e| BackupError::Transform(e.to_string()))?
                .as_bytes(),
        )?;

        Ok(Backup { meta, data_path })
    }

    /// Timestamped backups, newest first. Excludes the pristine copy.
    pub fn history(&self) -> Vec<Backup> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<Backup> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension()? != "json" || p.file_stem()? == "pristine" {
                    return None;
                }
                let meta: BackupMeta =
                    serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
                let data_path = p.with_extension("ini");
                Some(Backup { meta, data_path })
            })
            .collect();
        out.sort_by(|a, b| b.meta.id.cmp(&a.meta.id));
        out
    }

    /// Drop the oldest rolling backups past [`HISTORY_LIMIT`]. Never touches
    /// the pristine copy.
    fn prune(&self) -> Result<()> {
        for old in self.history().into_iter().skip(HISTORY_LIMIT) {
            let _ = std::fs::remove_file(&old.data_path);
            let _ = std::fs::remove_file(old.data_path.with_extension("json"));
        }
        Ok(())
    }

    /// Restore a backup over `target`, verifying it first.
    ///
    /// Works even if `target` has been deleted entirely.
    pub fn restore(&self, backup: &Backup, target: &Path) -> Result<()> {
        backup.verify()?;
        let bytes = std::fs::read(&backup.data_path).map_err(io(&backup.data_path))?;
        write_atomic(target, &bytes)?;

        let readback = std::fs::read(target).map_err(io(target))?;
        let got = sha256_hex(&readback);
        if got != backup.meta.sha256 {
            return Err(BackupError::VerifyFailed {
                path: target.to_path_buf(),
                written: readback.len() as u64,
                got,
                expected: backup.meta.sha256.clone(),
            });
        }
        Ok(())
    }
}

/// Write via a temp file and rename, so a crash cannot leave a truncated file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("dat")
    ));

    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(io(&tmp))?;
        f.write_all(bytes).map_err(io(&tmp))?;
        // Force to disk before the rename, so the rename cannot expose a file
        // whose contents are still buffered.
        f.sync_all().map_err(io(&tmp))?;
    }

    // Windows rename fails if the destination exists, so remove it first. The
    // temp file is already complete and fsynced at this point.
    if path.exists() {
        std::fs::remove_file(path).map_err(io(path))?;
    }
    std::fs::rename(&tmp, path).map_err(io(path))?;
    Ok(())
}

/// Report of a completed edit.
#[derive(Debug, Clone)]
pub struct EditReport {
    pub backup_id: String,
    pub backup_path: PathBuf,
    pub pristine_path: PathBuf,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub changed: bool,
}

/// A file that can only be written through a verified-backup path.
pub struct GuardedFile {
    target: PathBuf,
    store: BackupStore,
    /// Overridable so tests do not depend on whether Siege happens to be open.
    game_running: fn() -> Option<u32>,
}

impl GuardedFile {
    pub fn new(target: impl Into<PathBuf>, store: BackupStore) -> Self {
        GuardedFile {
            target: target.into(),
            store,
            game_running: crate::running_game_pid,
        }
    }

    #[doc(hidden)]
    pub fn with_game_check(mut self, f: fn() -> Option<u32>) -> Self {
        self.game_running = f;
        self
    }

    pub fn target(&self) -> &Path {
        &self.target
    }

    pub fn store(&self) -> &BackupStore {
        &self.store
    }

    /// Checks that must pass before anything is written.
    pub fn preflight(&self) -> Result<()> {
        if !self.target.is_file() {
            return Err(BackupError::Missing {
                path: self.target.clone(),
            });
        }
        let meta = std::fs::metadata(&self.target).map_err(io(&self.target))?;
        if meta.permissions().readonly() {
            return Err(BackupError::ReadOnly {
                path: self.target.clone(),
            });
        }
        if let Some(pid) = (self.game_running)() {
            return Err(BackupError::GameRunning { pid });
        }
        Ok(())
    }

    /// Read the file without touching it.
    pub fn read(&self) -> Result<String> {
        let bytes = std::fs::read(&self.target).map_err(io(&self.target))?;
        String::from_utf8(bytes).map_err(|_| BackupError::NotUtf8 {
            path: self.target.clone(),
        })
    }

    /// The only write path.
    ///
    /// `transform` receives the current contents and returns the new contents.
    /// It is not called until preflight has passed and both the pristine and a
    /// fresh timestamped backup exist and have been verified — so there is no
    /// ordering in which a write happens without a good backup behind it.
    pub fn edit<F>(&self, transform: F) -> Result<EditReport>
    where
        F: FnOnce(&str) -> std::result::Result<String, String>,
    {
        self.preflight()?;

        // 1. The permanent original, taken once, verified every time.
        let pristine = self.store.ensure_pristine(&self.target)?;
        pristine.verify()?;

        // 2. A fresh point-in-time copy, so this edit does not lean on pristine.
        let backup = self.store.take(&self.target)?;
        backup.verify()?;

        // 3. Only now is the caller's transform allowed to run.
        let before = self.read()?;
        let after = transform(&before).map_err(BackupError::Transform)?;

        if after == before {
            return Ok(EditReport {
                backup_id: backup.meta.id,
                backup_path: backup.data_path,
                pristine_path: pristine.data_path,
                bytes_before: before.len() as u64,
                bytes_after: after.len() as u64,
                changed: false,
            });
        }

        // 4. Atomic write; on failure put the backup back.
        if let Err(e) = write_atomic(&self.target, after.as_bytes()) {
            let _ = self.store.restore(&backup, &self.target);
            return Err(e);
        }

        // 5. Confirm what is on disk is what we intended.
        let readback = std::fs::read(&self.target).map_err(io(&self.target))?;
        if readback != after.as_bytes() {
            self.store.restore(&backup, &self.target)?;
            return Err(BackupError::VerifyFailed {
                path: self.target.clone(),
                written: readback.len() as u64,
                got: sha256_hex(&readback),
                expected: sha256_hex(after.as_bytes()),
            });
        }

        Ok(EditReport {
            backup_id: backup.meta.id,
            backup_path: backup.data_path,
            pristine_path: pristine.data_path,
            bytes_before: before.len() as u64,
            bytes_after: after.len() as u64,
            changed: true,
        })
    }

    /// Put the file back to a specific backup.
    pub fn restore(&self, backup: &Backup) -> Result<()> {
        self.store.restore(backup, &self.target)
    }

    /// Put the file back to how OPTEA first found it.
    pub fn restore_pristine(&self) -> Result<()> {
        let pristine = self
            .store
            .pristine()
            .ok_or_else(|| BackupError::Missing {
                path: self.store.pristine_data(),
            })?;
        self.store.restore(&pristine, &self.target)
    }
}

/// Sortable UTC id, `YYYYMMDD-HHMMSS-mmm`.
fn timestamp_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{:03}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        now.subsec_millis()
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Matches the real file: CRLF endings, no BOM.
    const SAMPLE: &str = "[GENERAL]\r\nRenderScale=100\r\nMaxGPUBufferedFrame=1\r\n";

    fn no_game() -> Option<u32> {
        None
    }
    fn game_at_1234() -> Option<u32> {
        Some(1234)
    }

    struct Fixture {
        _root: PathBuf,
        target: PathBuf,
        file: GuardedFile,
    }

    fn fixture(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("optea-backup-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let target = root.join("GameSettings.ini");
        std::fs::write(&target, SAMPLE).unwrap();

        let file = GuardedFile::new(target.clone(), BackupStore::new(root.join("backups")))
            .with_game_check(no_game);
        Fixture {
            _root: root,
            target,
            file,
        }
    }

    #[test]
    fn edit_creates_a_verified_backup_before_writing() {
        let fx = fixture("basic");
        let report = fx
            .file
            .edit(|s| Ok(s.replace("RenderScale=100", "RenderScale=80")))
            .unwrap();

        assert!(report.changed);
        assert!(report.backup_path.is_file(), "timestamped backup must exist");
        assert!(report.pristine_path.is_file(), "pristine must exist");

        // The backup holds the ORIGINAL, not the new content.
        assert_eq!(std::fs::read_to_string(&report.backup_path).unwrap(), SAMPLE);
        assert!(fx.file.read().unwrap().contains("RenderScale=80"));
    }

    #[test]
    fn transform_never_runs_without_a_backup() {
        // The ordering guarantee: if preflight fails, the transform must not
        // have been given a chance to run.
        let fx = fixture("no-transform");
        let guarded = GuardedFile::new(
            fx.target.clone(),
            BackupStore::new(fx._root.join("backups2")),
        )
        .with_game_check(game_at_1234);

        let mut ran = false;
        let err = guarded
            .edit(|s| {
                ran = true;
                Ok(s.to_string())
            })
            .unwrap_err();

        assert!(matches!(err, BackupError::GameRunning { pid: 1234 }));
        assert!(!ran, "transform ran despite preflight failing");
        assert_eq!(std::fs::read_to_string(&fx.target).unwrap(), SAMPLE);
    }

    #[test]
    fn refuses_to_edit_while_the_game_is_running() {
        let fx = fixture("game-running");
        let guarded = GuardedFile::new(fx.target.clone(), BackupStore::new(fx._root.join("b")))
            .with_game_check(game_at_1234);

        let err = guarded.edit(|_| Ok("clobbered".into())).unwrap_err();
        assert!(err.to_string().contains("rewrites GameSettings.ini"));
        assert_eq!(
            std::fs::read_to_string(&fx.target).unwrap(),
            SAMPLE,
            "file must be untouched"
        );
    }

    #[test]
    fn pristine_is_taken_once_and_never_overwritten() {
        let fx = fixture("pristine-once");

        fx.file.edit(|_| Ok("first edit\r\n".into())).unwrap();
        let p1 = fx.file.store().pristine().unwrap();
        assert_eq!(std::fs::read_to_string(&p1.data_path).unwrap(), SAMPLE);

        fx.file.edit(|_| Ok("second edit\r\n".into())).unwrap();
        let p2 = fx.file.store().pristine().unwrap();

        assert_eq!(p1.meta.id, p2.meta.id, "pristine id must not change");
        assert_eq!(
            std::fs::read_to_string(&p2.data_path).unwrap(),
            SAMPLE,
            "pristine must still hold the ORIGINAL after several edits"
        );
    }

    #[test]
    fn restore_pristine_recovers_the_original_after_many_edits() {
        let fx = fixture("restore-pristine");
        for i in 0..5 {
            fx.file.edit(|_| Ok(format!("edit {i}\r\n"))).unwrap();
        }
        assert_ne!(std::fs::read_to_string(&fx.target).unwrap(), SAMPLE);

        fx.file.restore_pristine().unwrap();
        assert_eq!(
            std::fs::read_to_string(&fx.target).unwrap(),
            SAMPLE,
            "must be byte-identical to the original"
        );
    }

    #[test]
    fn restore_works_even_if_the_file_was_deleted() {
        let fx = fixture("deleted");
        fx.file.edit(|_| Ok("changed\r\n".into())).unwrap();

        std::fs::remove_file(&fx.target).unwrap();
        assert!(!fx.target.exists());

        fx.file.restore_pristine().unwrap();
        assert_eq!(std::fs::read_to_string(&fx.target).unwrap(), SAMPLE);
    }

    #[test]
    fn a_failing_transform_leaves_the_file_untouched() {
        let fx = fixture("transform-fails");
        let err = fx
            .file
            .edit(|_| Err("could not parse the section".into()))
            .unwrap_err();

        assert!(matches!(err, BackupError::Transform(_)));
        assert_eq!(
            std::fs::read_to_string(&fx.target).unwrap(),
            SAMPLE,
            "a failed transform must not modify anything"
        );
        // A backup was still taken, so the original is recorded regardless.
        assert!(fx.file.store().pristine().is_some());
    }

    #[test]
    fn crlf_line_endings_survive_a_round_trip() {
        // Regression guard: the real file is 100% CRLF, and a naive
        // lines()/join("\n") rewrite would silently convert every one of them.
        let fx = fixture("crlf");
        fx.file
            .edit(|s| Ok(s.replace("RenderScale=100", "RenderScale=50")))
            .unwrap();

        let after = std::fs::read_to_string(&fx.target).unwrap();
        assert_eq!(after.matches("\r\n").count(), SAMPLE.matches("\r\n").count());
        assert!(
            !after.contains("\n\n") && !after.replace("\r\n", "").contains('\n'),
            "a bare LF appeared: {after:?}"
        );
    }

    #[test]
    fn backup_detects_tampering() {
        let fx = fixture("tamper");
        let report = fx.file.edit(|_| Ok("x\r\n".into())).unwrap();

        let backup = fx.file.store().history().into_iter().next().unwrap();
        backup.verify().unwrap();

        // Corrupt the stored backup behind the store's back.
        std::fs::write(&report.backup_path, "corrupted").unwrap();
        let err = backup.verify().unwrap_err();
        assert!(
            matches!(err, BackupError::VerifyFailed { .. }),
            "a corrupted backup must not verify: {err:?}"
        );
    }

    #[test]
    fn unchanged_content_is_reported_without_a_write() {
        let fx = fixture("noop");
        let before = std::fs::metadata(&fx.target).unwrap().len();
        let report = fx.file.edit(|s| Ok(s.to_string())).unwrap();

        assert!(!report.changed);
        assert_eq!(std::fs::metadata(&fx.target).unwrap().len(), before);
        assert_eq!(std::fs::read_to_string(&fx.target).unwrap(), SAMPLE);
    }

    #[test]
    fn history_accumulates_and_is_newest_first() {
        let fx = fixture("history");
        for i in 0..3 {
            fx.file.edit(|_| Ok(format!("v{i}\r\n"))).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let history = fx.file.store().history();
        assert_eq!(history.len(), 3);
        assert!(
            history[0].meta.id > history[1].meta.id,
            "history must be newest first"
        );
        // Pristine is not part of the rolling history.
        assert!(history.iter().all(|b| !b.meta.pristine));
    }

    #[test]
    fn every_backup_in_history_verifies() {
        let fx = fixture("verify-all");
        for i in 0..4 {
            fx.file.edit(|_| Ok(format!("n{i}\r\n"))).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        for b in fx.file.store().history() {
            b.verify().unwrap();
        }
        fx.file.store().pristine().unwrap().verify().unwrap();
    }

    #[test]
    fn missing_target_is_reported_clearly() {
        let root = std::env::temp_dir().join("optea-backup-missing");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let f = GuardedFile::new(root.join("nope.ini"), BackupStore::new(root.join("b")))
            .with_game_check(no_game);
        assert!(matches!(
            f.edit(|s| Ok(s.into())).unwrap_err(),
            BackupError::Missing { .. }
        ));
    }

    #[test]
    fn atomic_write_leaves_no_temp_file_behind() {
        let fx = fixture("no-temp");
        fx.file.edit(|_| Ok("done\r\n".into())).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(fx.target.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.path().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn sha256_matches_known_vector() {
        // Guards against a hash change silently invalidating every stored backup.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
