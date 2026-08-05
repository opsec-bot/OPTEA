//! Transactional apply and revert.
//!
//! Two guarantees, both tested:
//!
//! 1. **Capture precedes apply.** [`Engine::apply`] persists a snapshot to disk
//!    *before* calling [`Tweak::apply`], so a crash between the two still leaves
//!    a recoverable record on disk.
//! 2. **Partial applies roll back.** If tweak 7 of 12 fails, the six already
//!    applied are restored in reverse order before the error surfaces. A
//!    half-applied profile is the state most likely to leave someone with a
//!    machine that behaves oddly and no idea which change did it.

use crate::tweak::{Applicability, Risk, Snapshot, SystemInfo, Tweak};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One tweak's captured state within a transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub tweak_id: String,
    pub snapshot: Snapshot,
    /// Whether [`Tweak::apply`] actually completed for this entry.
    pub applied: bool,
}

/// A persisted record of one `apply` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    /// UTC timestamp, also used as the file name.
    pub id: String,
    pub profile: String,
    pub entries: Vec<SnapshotEntry>,
    /// True once every entry has been reverted.
    pub reverted: bool,
    pub requires_reboot: bool,
}

impl Transaction {
    pub fn applied_ids(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| e.applied)
            .map(|e| e.tweak_id.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum Outcome {
    Applied,
    /// Skipped because it was already in the desired state.
    AlreadySet,
    Skipped { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub transaction_id: String,
    pub outcomes: BTreeMap<String, Outcome>,
    pub requires_reboot: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{tweak}: apply failed: {source}. All changes in this run were rolled back.")]
    AppliedAndRolledBack {
        tweak: String,
        #[source]
        source: anyhow::Error,
    },

    #[error(
        "{tweak}: apply failed AND rollback failed: {rollback}. The system may be in a mixed \
         state. Snapshot {snapshot} on disk records the original values."
    )]
    RollbackFailed {
        tweak: String,
        rollback: String,
        snapshot: String,
    },

    #[error("{0} requires administrator rights")]
    NeedsElevation(String),

    #[error(
        "{tweak} is a deep-risk change and needs a system restore point, which could not be \
         created: {reason}"
    )]
    NoRestorePoint { tweak: String, reason: String },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("snapshot {0} not found")]
    NoSuchSnapshot(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

type Result<T> = std::result::Result<T, EngineError>;

/// Applies and reverts tweaks, persisting snapshots under `snapshot_dir`.
pub struct Engine {
    snapshot_dir: PathBuf,
    sys: SystemInfo,
    /// When true, deep-risk tweaks are refused outright.
    allow_deep: bool,
}

impl Engine {
    pub fn new(snapshot_dir: impl Into<PathBuf>, sys: SystemInfo) -> Self {
        Engine {
            snapshot_dir: snapshot_dir.into(),
            sys,
            allow_deep: false,
        }
    }

    /// Default location: `%ProgramData%\OPTEA\snapshots`.
    pub fn with_default_dir(sys: SystemInfo) -> anyhow::Result<Self> {
        Ok(Engine::new(
            optea_sys::paths::optea_data_dir()?.join("snapshots"),
            sys,
        ))
    }

    /// Opt in to deep-risk tweaks. Off by default.
    pub fn allow_deep(mut self, allow: bool) -> Self {
        self.allow_deep = allow;
        self
    }

    pub fn snapshot_dir(&self) -> &Path {
        &self.snapshot_dir
    }

    /// Apply a set of tweaks as one transaction.
    ///
    /// On any failure, every tweak already applied in this run is restored in
    /// reverse order and the original error is returned.
    pub fn apply(&self, profile: &str, tweaks: &[&dyn Tweak]) -> Result<ApplyResult> {
        let id = timestamp_id();
        let mut tx = Transaction {
            id: id.clone(),
            profile: profile.to_string(),
            entries: Vec::new(),
            reverted: false,
            requires_reboot: false,
        };
        let mut outcomes = BTreeMap::new();

        for tweak in tweaks {
            match tweak.applicable(&self.sys) {
                Applicability::AlreadySet => {
                    outcomes.insert(tweak.id().to_string(), Outcome::AlreadySet);
                    continue;
                }
                Applicability::NotApplicable { reason } => {
                    outcomes.insert(tweak.id().to_string(), Outcome::Skipped { reason });
                    continue;
                }
                Applicability::Applicable => {}
            }

            if tweak.risk() == Risk::Deep && !self.allow_deep {
                outcomes.insert(
                    tweak.id().to_string(),
                    Outcome::Skipped {
                        reason: "deep-risk tweaks require an explicit opt-in".into(),
                    },
                );
                continue;
            }

            // Capture and persist BEFORE applying, so an interruption between
            // the two still leaves a usable record on disk.
            let snapshot = tweak.capture()?;
            tx.entries.push(SnapshotEntry {
                tweak_id: tweak.id().to_string(),
                snapshot,
                applied: false,
            });
            self.persist(&tx)?;

            match tweak.apply() {
                Ok(()) => {
                    if let Some(last) = tx.entries.last_mut() {
                        last.applied = true;
                    }
                    tx.requires_reboot |= tweak.requires_reboot();
                    self.persist(&tx)?;
                    outcomes.insert(tweak.id().to_string(), Outcome::Applied);
                }
                Err(source) => {
                    // Undo everything this run already did.
                    if let Err(rollback) = self.rollback(&mut tx, tweaks) {
                        return Err(EngineError::RollbackFailed {
                            tweak: tweak.id().to_string(),
                            rollback: rollback.to_string(),
                            snapshot: self.snapshot_path(&id).display().to_string(),
                        });
                    }
                    return Err(EngineError::AppliedAndRolledBack {
                        tweak: tweak.id().to_string(),
                        source,
                    });
                }
            }
        }

        self.persist(&tx)?;
        Ok(ApplyResult {
            transaction_id: id,
            outcomes,
            requires_reboot: tx.requires_reboot,
        })
    }

    /// Restore every applied entry, most recent first.
    fn rollback(&self, tx: &mut Transaction, tweaks: &[&dyn Tweak]) -> Result<()> {
        let mut failures = Vec::new();

        for entry in tx.entries.iter_mut().rev() {
            if !entry.applied {
                continue;
            }
            let Some(tweak) = tweaks.iter().find(|t| t.id() == entry.tweak_id) else {
                failures.push(format!("{}: no tweak with this id", entry.tweak_id));
                continue;
            };
            // Keep going after a failure: one stuck tweak must not strand the rest.
            match tweak.restore(&entry.snapshot) {
                Ok(()) => entry.applied = false,
                Err(e) => failures.push(format!("{}: {e}", entry.tweak_id)),
            }
        }

        tx.reverted = failures.is_empty();
        self.persist(tx)?;

        if failures.is_empty() {
            Ok(())
        } else {
            Err(EngineError::Other(anyhow::anyhow!(failures.join("; "))))
        }
    }

    /// Revert a previously applied transaction.
    pub fn revert(&self, transaction_id: &str, tweaks: &[&dyn Tweak]) -> Result<Vec<String>> {
        let mut tx = self.load(transaction_id)?;
        let reverted: Vec<String> = tx
            .entries
            .iter()
            .filter(|e| e.applied)
            .map(|e| e.tweak_id.clone())
            .collect();
        self.rollback(&mut tx, tweaks)?;
        Ok(reverted)
    }

    /// Most recent transaction id, if any.
    pub fn latest_transaction(&self) -> Option<String> {
        let mut ids: Vec<String> = self.list_transactions().ok()?;
        ids.sort();
        ids.pop()
    }

    pub fn list_transactions(&self) -> Result<Vec<String>> {
        if !self.snapshot_dir.is_dir() {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(&self.snapshot_dir).map_err(|source| EngineError::Io {
            path: self.snapshot_dir.clone(),
            source,
        })?;
        Ok(entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                (p.extension()? == "json")
                    .then(|| p.file_stem()?.to_str().map(str::to_owned))
                    .flatten()
            })
            .collect())
    }

    pub fn load(&self, transaction_id: &str) -> Result<Transaction> {
        let path = self.snapshot_path(transaction_id);
        if !path.is_file() {
            return Err(EngineError::NoSuchSnapshot(transaction_id.to_string()));
        }
        let text = std::fs::read_to_string(&path).map_err(|source| EngineError::Io {
            path: path.clone(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|e| EngineError::Other(e.into()))
    }

    fn snapshot_path(&self, id: &str) -> PathBuf {
        self.snapshot_dir.join(format!("{id}.json"))
    }

    /// Write the transaction to disk atomically.
    ///
    /// Writes to a temp file and renames, so a crash mid-write cannot leave a
    /// truncated snapshot — which would be worse than no snapshot, since the
    /// user would believe they had a way back.
    fn persist(&self, tx: &Transaction) -> Result<()> {
        std::fs::create_dir_all(&self.snapshot_dir).map_err(|source| EngineError::Io {
            path: self.snapshot_dir.clone(),
            source,
        })?;

        let final_path = self.snapshot_path(&tx.id);
        let tmp_path = final_path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(tx).map_err(|e| EngineError::Other(e.into()))?;

        std::fs::write(&tmp_path, json).map_err(|source| EngineError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &final_path).map_err(|source| EngineError::Io {
            path: final_path,
            source,
        })?;
        Ok(())
    }
}

/// Sortable UTC id: `20260804-193045-123`.
fn timestamp_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    // Civil-from-days, so snapshot names stay human-readable without pulling a
    // date library into the engine.
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{millis:03}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Shared with [`crate::bench`], which needs the same id format.
pub(crate) fn civil_from_days_pub(z: i64) -> (i64, u32, u32) {
    civil_from_days(z)
}

/// Howard Hinnant's `civil_from_days`, for days since the Unix epoch.
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
    use crate::tweak::{Evidence, RegistryTweak};
    use optea_sys::registry::{self, RegKey, RegValue, Root};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("optea-engine-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn key(name: &str) -> RegKey {
        RegKey::new(Root::Hkcu, r"Software\OPTEA\EngineScratch", name)
    }

    fn reg_tweak(id: &'static str, name: &str, desired: u32) -> RegistryTweak {
        RegistryTweak {
            id,
            title: "scratch",
            description: "scratch",
            risk: Risk::Safe,
            requires_reboot: false,
            key: key(name),
            desired: RegValue::Dword(desired),
            gate: |_| Applicability::Applicable,
        }
    }

    /// A tweak that always fails to apply, to drive the rollback path.
    struct FailingTweak {
        id: &'static str,
        captures: AtomicUsize,
    }

    impl FailingTweak {
        fn new(id: &'static str) -> Self {
            FailingTweak {
                id,
                captures: AtomicUsize::new(0),
            }
        }
    }

    impl Tweak for FailingTweak {
        fn id(&self) -> &'static str {
            self.id
        }
        fn title(&self) -> &'static str {
            "always fails"
        }
        fn description(&self) -> &'static str {
            "test fixture"
        }
        fn risk(&self) -> Risk {
            Risk::Safe
        }
        fn applicable(&self, _: &SystemInfo) -> Applicability {
            Applicability::Applicable
        }
        fn probe(&self) -> anyhow::Result<String> {
            Ok("n/a".into())
        }
        fn capture(&self) -> anyhow::Result<Snapshot> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            Ok(Snapshot::PowerScheme {
                guid: "fixture".into(),
            })
        }
        fn apply(&self) -> anyhow::Result<()> {
            anyhow::bail!("simulated failure")
        }
        fn restore(&self, _: &Snapshot) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn sys() -> SystemInfo {
        SystemInfo::query().unwrap()
    }

    #[test]
    fn applies_and_records_a_transaction() {
        let dir = scratch_dir("apply");
        let engine = Engine::new(&dir, sys());
        let a = reg_tweak("scratch_a", "apply_a", 1);
        let b = reg_tweak("scratch_b", "apply_b", 2);
        let _ = registry::delete(&a.key);
        let _ = registry::delete(&b.key);

        let result = engine.apply("test", &[&a, &b]).unwrap();
        assert!(matches!(result.outcomes["scratch_a"], Outcome::Applied));
        assert!(matches!(result.outcomes["scratch_b"], Outcome::Applied));
        assert_eq!(registry::read(&a.key).unwrap(), Some(RegValue::Dword(1)));

        // The transaction is on disk and reloadable.
        let tx = engine.load(&result.transaction_id).unwrap();
        assert_eq!(tx.applied_ids(), vec!["scratch_a", "scratch_b"]);

        engine.revert(&result.transaction_id, &[&a, &b]).unwrap();
        assert_eq!(registry::read(&a.key).unwrap(), None);
        assert_eq!(registry::read(&b.key).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Plan verification step 2: a mid-profile failure must roll back cleanly.
    #[test]
    fn mid_profile_failure_rolls_back_everything() {
        let dir = scratch_dir("rollback");
        let engine = Engine::new(&dir, sys());

        let a = reg_tweak("roll_a", "roll_a", 11);
        let b = reg_tweak("roll_b", "roll_b", 22);
        let boom = FailingTweak::new("roll_boom");
        let c = reg_tweak("roll_c", "roll_c", 33);

        // Pre-existing values that must be restored exactly.
        registry::write(&a.key, &RegValue::Dword(1)).unwrap();
        let _ = registry::delete(&b.key); // absent: revert must delete
        let _ = registry::delete(&c.key);

        let tweaks: Vec<&dyn Tweak> = vec![&a, &b, &boom, &c];
        let err = engine.apply("test", &tweaks).unwrap_err();
        assert!(
            matches!(err, EngineError::AppliedAndRolledBack { .. }),
            "got {err:?}"
        );

        assert_eq!(
            registry::read(&a.key).unwrap(),
            Some(RegValue::Dword(1)),
            "pre-existing value must be restored to its original"
        );
        assert_eq!(
            registry::read(&b.key).unwrap(),
            None,
            "value that did not exist must be deleted again"
        );
        assert_eq!(
            registry::read(&c.key).unwrap(),
            None,
            "tweak after the failure must never have been applied"
        );

        registry::delete(&a.key).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_is_persisted_before_apply() {
        // Capture must reach disk before the change happens, so an interruption
        // still leaves a way back.
        let dir = scratch_dir("persist-order");
        let engine = Engine::new(&dir, sys());
        let boom = FailingTweak::new("persist_boom");

        let tweaks: Vec<&dyn Tweak> = vec![&boom];
        let _ = engine.apply("test", &tweaks).unwrap_err();

        assert_eq!(boom.captures.load(Ordering::SeqCst), 1, "capture must run");
        let ids = engine.list_transactions().unwrap();
        assert_eq!(ids.len(), 1, "a snapshot file must exist despite the failure");

        let tx = engine.load(&ids[0]).unwrap();
        assert_eq!(tx.entries.len(), 1);
        assert!(!tx.entries[0].applied, "failed apply must not be marked applied");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn already_set_tweaks_are_not_reapplied() {
        let dir = scratch_dir("alreadyset");
        let engine = Engine::new(&dir, sys());
        let t = reg_tweak("already", "already", 5);
        registry::write(&t.key, &RegValue::Dword(5)).unwrap();

        let result = engine.apply("test", &[&t]).unwrap();
        assert!(matches!(result.outcomes["already"], Outcome::AlreadySet));

        let tx = engine.load(&result.transaction_id).unwrap();
        assert!(tx.entries.is_empty(), "no snapshot needed for a no-op");

        registry::delete(&t.key).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deep_tweaks_are_skipped_without_opt_in() {
        let dir = scratch_dir("deep");
        let mut t = reg_tweak("deep_one", "deep_one", 1);
        t.risk = Risk::Deep;
        let _ = registry::delete(&t.key);

        let engine = Engine::new(&dir, sys());
        let result = engine.apply("test", &[&t]).unwrap();
        assert!(
            matches!(result.outcomes["deep_one"], Outcome::Skipped { .. }),
            "deep tweak must be skipped by default"
        );
        assert_eq!(registry::read(&t.key).unwrap(), None);

        // With the opt-in it goes through.
        let engine = Engine::new(&dir, sys()).allow_deep(true);
        let result = engine.apply("test", &[&t]).unwrap();
        assert!(matches!(result.outcomes["deep_one"], Outcome::Applied));

        engine.revert(&result.transaction_id, &[&t]).unwrap();
        assert_eq!(registry::read(&t.key).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reboot_requirement_propagates() {
        let dir = scratch_dir("reboot");
        let engine = Engine::new(&dir, sys());
        let mut t = reg_tweak("needs_reboot", "needs_reboot", 1);
        t.requires_reboot = true;
        let _ = registry::delete(&t.key);

        let result = engine.apply("test", &[&t]).unwrap();
        assert!(result.requires_reboot);

        engine.revert(&result.transaction_id, &[&t]).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transaction_ids_sort_chronologically() {
        let a = timestamp_id();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let b = timestamp_id();
        assert!(a < b, "{a} should sort before {b}");
        assert_eq!(a.len(), b.len(), "ids must be fixed-width to sort correctly");
    }

    #[test]
    fn timestamp_id_has_expected_shape() {
        let id = timestamp_id();
        // YYYYMMDD-HHMMSS-mmm
        assert_eq!(id.len(), 19, "{id}");
        assert_eq!(&id[8..9], "-");
        assert_eq!(&id[15..16], "-");
        let year: i64 = id[0..4].parse().unwrap();
        assert!((2024..2100).contains(&year), "implausible year in {id}");
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_638), (2026, 7, 4));
    }

    #[test]
    fn evidence_defaults_to_unverified() {
        // Nothing enters the catalog claiming to be proven.
        let t = reg_tweak("ev", "ev", 1);
        assert_eq!(t.evidence(), Evidence::Unverified);
    }

    #[test]
    fn missing_snapshot_is_reported_clearly() {
        let engine = Engine::new(scratch_dir("missing"), sys());
        let err = engine.load("nope").unwrap_err();
        assert!(matches!(err, EngineError::NoSuchSnapshot(_)));
    }
}
