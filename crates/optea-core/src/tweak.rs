//! The tweak model.
//!
//! Design rule: **a tweak that cannot be captured cannot be applied.** The
//! engine always calls [`Tweak::capture`] and persists the result before
//! [`Tweak::apply`] runs, so every change has a recorded inverse before it
//! happens. This is enforced by the engine rather than left to each tweak's
//! good behaviour, because "I forgot to save the old value" is how an optimizer
//! permanently alters someone's machine.

use optea_sys::registry::{RegKey, RegValue};
use optea_sys::sysinfo::{CpuInfo, OsInfo};
use serde::{Deserialize, Serialize};

/// How much damage a tweak can do if it goes wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Reversible, user-visible, cannot prevent boot.
    Safe,
    /// Touches scheduler or driver behaviour. Reversible, but benchmark it.
    Moderate,
    /// Kernel- or boot-adjacent. Can render a machine unbootable. Requires a
    /// restore point and an explicit opt-in.
    Deep,
}

impl Risk {
    pub fn label(&self) -> &'static str {
        match self {
            Risk::Safe => "safe",
            Risk::Moderate => "moderate",
            Risk::Deep => "deep",
        }
    }

    /// Deep tweaks must not be applied without a system restore point.
    pub fn requires_restore_point(&self) -> bool {
        matches!(self, Risk::Deep)
    }
}

/// How much OPTEA actually knows about whether a tweak helps *on this machine*.
///
/// Everything ships as [`Evidence::Unverified`]. Promotion to
/// [`Evidence::Measured`] happens only through the benchmark harness — never by
/// citing a forum post.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// Benchmarked on this system with a confidence interval excluding zero.
    Measured,
    /// Plausible and commonly recommended, but unproven here.
    Unverified,
    /// Known to do nothing on this configuration. Kept visible so the user is
    /// not tempted to apply it from some guide.
    KnownNoop,
}

impl Evidence {
    pub fn label(&self) -> &'static str {
        match self {
            Evidence::Measured => "measured",
            Evidence::Unverified => "unverified",
            Evidence::KnownNoop => "known no-op",
        }
    }
}

/// Whether a tweak means anything on this system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Applicability {
    /// Can be applied.
    Applicable,
    /// Already in the desired state; applying would be a no-op.
    AlreadySet,
    /// Cannot work here, with the reason stated in the user's terms.
    NotApplicable { reason: String },
}

impl Applicability {
    pub fn not_applicable(reason: impl Into<String>) -> Self {
        Applicability::NotApplicable {
            reason: reason.into(),
        }
    }

    pub fn is_applicable(&self) -> bool {
        matches!(self, Applicability::Applicable)
    }
}

/// System facts a tweak consults to decide whether it applies.
#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub os: OsInfo,
    pub cpu: CpuInfo,
    pub has_nvidia_gpu: bool,
}

impl SystemInfo {
    pub fn query() -> anyhow::Result<Self> {
        let gpus = optea_sys::gpu::enumerate().unwrap_or_default();
        Ok(SystemInfo {
            os: OsInfo::query()?,
            cpu: CpuInfo::query()?,
            has_nvidia_gpu: gpus.iter().any(|g| g.is_nvidia() && g.is_healthy()),
        })
    }
}

/// A captured prior state, sufficient to reverse an apply.
///
/// `Registry` deliberately stores `Option<RegValue>`: `None` means the value did
/// not exist, and restoring it means *deleting* rather than zeroing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Snapshot {
    Registry {
        key: RegKey,
        value: Option<RegValue>,
    },
    /// Several registry values captured together, restored in reverse order.
    RegistrySet { entries: Vec<(RegKey, Option<RegValue>)> },
    /// Active power scheme GUID.
    PowerScheme { guid: String },
    /// A file copied aside before modification.
    FileBackup { original: String, backup: String },
}

/// Something OPTEA can turn on, off, and — always — back again.
pub trait Tweak: Send + Sync {
    /// Stable identifier used in profiles, snapshots, and benchmark results.
    fn id(&self) -> &'static str;

    fn title(&self) -> &'static str;

    /// What this changes and why someone would want it.
    fn description(&self) -> &'static str;

    fn risk(&self) -> Risk;

    /// Defaults to `Unverified`; the benchmark harness promotes it.
    fn evidence(&self) -> Evidence {
        Evidence::Unverified
    }

    /// Whether the change only takes effect after a restart.
    fn requires_reboot(&self) -> bool {
        false
    }

    /// Whether this tweak means anything on the current system.
    fn applicable(&self, sys: &SystemInfo) -> Applicability;

    /// Current state, for reporting. Must not mutate anything.
    fn probe(&self) -> anyhow::Result<String>;

    /// Record enough state to undo an apply. Called before every apply.
    fn capture(&self) -> anyhow::Result<Snapshot>;

    fn apply(&self) -> anyhow::Result<()>;

    /// Reverse an apply using a snapshot from [`Tweak::capture`].
    fn restore(&self, snapshot: &Snapshot) -> anyhow::Result<()>;
}

/// A registry-backed tweak, which covers most of the catalog.
pub struct RegistryTweak {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub risk: Risk,
    pub requires_reboot: bool,
    pub key: RegKey,
    pub desired: RegValue,
    /// Gate on system facts; return `Applicable` when the tweak makes sense.
    pub gate: fn(&SystemInfo) -> Applicability,
}

impl Tweak for RegistryTweak {
    fn id(&self) -> &'static str {
        self.id
    }

    fn title(&self) -> &'static str {
        self.title
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn risk(&self) -> Risk {
        self.risk
    }

    fn requires_reboot(&self) -> bool {
        self.requires_reboot
    }

    fn applicable(&self, sys: &SystemInfo) -> Applicability {
        match (self.gate)(sys) {
            Applicability::Applicable => {
                // Report "already set" rather than performing a pointless write.
                match optea_sys::registry::read(&self.key) {
                    Ok(Some(v)) if v == self.desired => Applicability::AlreadySet,
                    _ => Applicability::Applicable,
                }
            }
            other => other,
        }
    }

    fn probe(&self) -> anyhow::Result<String> {
        Ok(match optea_sys::registry::read(&self.key)? {
            Some(v) => format!("{v:?}"),
            None => "not set".into(),
        })
    }

    fn capture(&self) -> anyhow::Result<Snapshot> {
        Ok(Snapshot::Registry {
            key: self.key.clone(),
            value: optea_sys::registry::read(&self.key)?,
        })
    }

    fn apply(&self) -> anyhow::Result<()> {
        optea_sys::registry::write(&self.key, &self.desired)?;
        Ok(())
    }

    fn restore(&self, snapshot: &Snapshot) -> anyhow::Result<()> {
        match snapshot {
            Snapshot::Registry { key, value } => {
                optea_sys::registry::restore(key, value)?;
                Ok(())
            }
            other => anyhow::bail!("{}: wrong snapshot kind {other:?}", self.id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use optea_sys::registry::Root;

    fn always_applicable(_: &SystemInfo) -> Applicability {
        Applicability::Applicable
    }

    fn test_tweak(name: &'static str, desired: RegValue) -> RegistryTweak {
        RegistryTweak {
            id: name,
            title: "Test tweak",
            description: "Scratch value used by the test suite.",
            risk: Risk::Safe,
            requires_reboot: false,
            key: RegKey::new(Root::Hkcu, r"Software\OPTEA\TweakScratch", name),
            desired,
            gate: always_applicable,
        }
    }

    fn sys() -> SystemInfo {
        SystemInfo::query().unwrap()
    }

    #[test]
    fn capture_apply_restore_round_trips_an_absent_value() {
        let t = test_tweak("absent_case", RegValue::Dword(0x26));
        let _ = optea_sys::registry::delete(&t.key);

        let snap = t.capture().unwrap();
        assert_eq!(snap, Snapshot::Registry { key: t.key.clone(), value: None });

        t.apply().unwrap();
        assert_eq!(
            optea_sys::registry::read(&t.key).unwrap(),
            Some(RegValue::Dword(0x26))
        );

        t.restore(&snap).unwrap();
        assert_eq!(
            optea_sys::registry::read(&t.key).unwrap(),
            None,
            "restoring an absent capture must delete the value"
        );
    }

    #[test]
    fn capture_apply_restore_round_trips_an_existing_value() {
        let t = test_tweak("existing_case", RegValue::Dword(0x26));
        optea_sys::registry::write(&t.key, &RegValue::Dword(2)).unwrap();

        let snap = t.capture().unwrap();
        t.apply().unwrap();
        t.restore(&snap).unwrap();

        assert_eq!(
            optea_sys::registry::read(&t.key).unwrap(),
            Some(RegValue::Dword(2))
        );
        optea_sys::registry::delete(&t.key).unwrap();
    }

    #[test]
    fn already_set_is_reported_rather_than_rewritten() {
        let t = test_tweak("already_set", RegValue::Dword(7));
        optea_sys::registry::write(&t.key, &RegValue::Dword(7)).unwrap();
        assert_eq!(t.applicable(&sys()), Applicability::AlreadySet);

        optea_sys::registry::write(&t.key, &RegValue::Dword(1)).unwrap();
        assert_eq!(t.applicable(&sys()), Applicability::Applicable);

        optea_sys::registry::delete(&t.key).unwrap();
    }

    #[test]
    fn gate_can_veto_a_tweak() {
        let mut t = test_tweak("gated", RegValue::Dword(1));
        t.gate = |_| Applicability::not_applicable("requires Windows 11");
        match t.applicable(&sys()) {
            Applicability::NotApplicable { reason } => assert!(reason.contains("Windows 11")),
            other => panic!("expected NotApplicable, got {other:?}"),
        }
    }

    #[test]
    fn restore_rejects_a_mismatched_snapshot() {
        let t = test_tweak("mismatch", RegValue::Dword(1));
        let wrong = Snapshot::PowerScheme {
            guid: "whatever".into(),
        };
        assert!(t.restore(&wrong).is_err(), "must not silently accept");
    }

    #[test]
    fn deep_risk_demands_a_restore_point() {
        assert!(Risk::Deep.requires_restore_point());
        assert!(!Risk::Moderate.requires_restore_point());
        assert!(!Risk::Safe.requires_restore_point());
    }

    #[test]
    fn risk_orders_from_safe_to_deep() {
        assert!(Risk::Safe < Risk::Moderate);
        assert!(Risk::Moderate < Risk::Deep);
    }

    #[test]
    fn snapshots_round_trip_through_json() {
        // Snapshots persist to disk and must survive a reboot to be useful.
        let snap = Snapshot::Registry {
            key: RegKey::hklm(r"SYSTEM\Test", "Value"),
            value: Some(RegValue::Dword(42)),
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert_eq!(serde_json::from_str::<Snapshot>(&json).unwrap(), snap);

        // Absence must survive serialisation too — this is the property that
        // makes revert correct.
        let absent = Snapshot::Registry {
            key: RegKey::hklm(r"SYSTEM\Test", "Value"),
            value: None,
        };
        let json = serde_json::to_string(&absent).unwrap();
        assert_eq!(serde_json::from_str::<Snapshot>(&json).unwrap(), absent);
    }
}
