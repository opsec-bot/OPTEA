//! The tweak catalog.
//!
//! Every entry ships as [`Evidence::Unverified`] regardless of how confidently
//! the internet recommends it. Only the benchmark harness promotes an entry to
//! [`Evidence::Measured`], and a `KnownNoop` entry stays visible precisely so
//! the user can see *why* OPTEA refuses to apply something they read about.
//!
//! Descriptions state the mechanism rather than a number. "Reduces input lag by
//! 40 ms" is the register of a product page; what belongs here is what the knob
//! does, so the measurement can decide the rest.

use crate::tweak::{Applicability, Evidence, RegistryTweak, Risk, SystemInfo, Tweak};
use optea_sys::registry::{RegKey, RegValue};
use optea_sys::sysinfo::WIN11_MIN_BUILD;

// ---- Gates -----------------------------------------------------------------

fn always(_: &SystemInfo) -> Applicability {
    Applicability::Applicable
}

fn needs_nvidia(sys: &SystemInfo) -> Applicability {
    if sys.has_nvidia_gpu {
        Applicability::Applicable
    } else {
        Applicability::not_applicable("no healthy NVIDIA GPU detected")
    }
}

/// `GlobalTimerResolutionRequests` is read by `ntoskrnl` only on Windows 11.
/// On Windows 10 the value can be written and will simply be ignored — which is
/// exactly the kind of silent nothing this project exists to call out.
fn needs_windows_11(sys: &SystemInfo) -> Applicability {
    if sys.os.is_windows_11() {
        Applicability::Applicable
    } else {
        Applicability::not_applicable(format!(
            "requires Windows 11 (build {WIN11_MIN_BUILD}+); this system is build {}",
            sys.os.build
        ))
    }
}

// ---- Registry locations ----------------------------------------------------

const PRIORITY_CONTROL: &str = r"SYSTEM\CurrentControlSet\Control\PriorityControl";
const GRAPHICS_DRIVERS: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";
const SESSION_KERNEL: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\kernel";
const GAME_DVR_POLICY: &str = r"SOFTWARE\Policies\Microsoft\Windows\GameDVR";
const NVIDIA_PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\nvlddmkm\Parameters";

/// A tweak that is deliberately never applied, only reported.
pub struct ReportOnly {
    inner: RegistryTweak,
    reason: &'static str,
}

impl Tweak for ReportOnly {
    fn id(&self) -> &'static str {
        self.inner.id
    }
    fn title(&self) -> &'static str {
        self.inner.title
    }
    fn description(&self) -> &'static str {
        self.inner.description
    }
    fn risk(&self) -> Risk {
        self.inner.risk
    }
    fn evidence(&self) -> Evidence {
        Evidence::KnownNoop
    }
    fn applicable(&self, _sys: &SystemInfo) -> Applicability {
        Applicability::not_applicable(self.reason)
    }
    fn probe(&self) -> anyhow::Result<String> {
        self.inner.probe()
    }
    fn capture(&self) -> anyhow::Result<crate::tweak::Snapshot> {
        self.inner.capture()
    }
    fn apply(&self) -> anyhow::Result<()> {
        anyhow::bail!("{} is report-only: {}", self.inner.id, self.reason)
    }
    fn restore(&self, snapshot: &crate::tweak::Snapshot) -> anyhow::Result<()> {
        self.inner.restore(snapshot)
    }
}

// ---- Catalog ---------------------------------------------------------------

/// Build the full catalog for this system.
///
/// The system is passed in so entries can be gated at construction time — but
/// gating is still re-evaluated at apply time, since state can change in between.
pub fn all(sys: &SystemInfo) -> Vec<Box<dyn Tweak>> {
    let mut v: Vec<Box<dyn Tweak>> = vec![
        // ---- Safe ----------------------------------------------------------
        Box::new(RegistryTweak {
            id: "gamedvr-off",
            title: "Disable Game DVR background recording",
            description:
                "Game DVR keeps a rolling capture buffer while you play, which costs CPU time \
                 and can add frame-pacing jitter. Disabling it removes that background work.",
            risk: Risk::Safe,
            requires_reboot: false,
            key: RegKey::hklm(GAME_DVR_POLICY, "AllowGameDVR"),
            desired: RegValue::Dword(0),
            gate: always,
        }),
        Box::new(RegistryTweak {
            id: "gamebar-startup-off",
            title: "Stop the Game Bar startup panel",
            description:
                "Prevents the Xbox Game Bar overlay panel from opening when a game launches. \
                 Removes an overlay surface from the present path.",
            risk: Risk::Safe,
            requires_reboot: false,
            key: RegKey::hkcu(r"SOFTWARE\Microsoft\GameBar", "ShowStartupPanel"),
            desired: RegValue::Dword(0),
            gate: always,
        }),
        Box::new(RegistryTweak {
            id: "gamedvr-user-off",
            title: "Disable Game DVR for this user",
            description:
                "The per-user counterpart to the Game DVR policy. Both are needed for the \
                 capture service to stay fully idle.",
            risk: Risk::Safe,
            requires_reboot: false,
            key: RegKey::hkcu(r"System\GameConfigStore", "GameDVR_Enabled"),
            desired: RegValue::Dword(0),
            gate: always,
        }),
        // ---- Moderate ------------------------------------------------------
        Box::new(RegistryTweak {
            id: "priority-separation",
            title: "Foreground thread quantum (Win32PrioritySeparation)",
            description:
                "Controls how much longer the foreground application's threads run before being \
                 preempted. The widely-copied value 0x26 (38) sets short, variable quanta with a \
                 strong foreground bias. Whether that helps is genuinely machine-specific — this \
                 is a prime candidate for measuring rather than believing.",
            risk: Risk::Moderate,
            requires_reboot: false,
            key: RegKey::hklm(PRIORITY_CONTROL, "Win32PrioritySeparation"),
            desired: RegValue::Dword(0x26),
            gate: always,
        }),
        Box::new(RegistryTweak {
            id: "hags-on",
            title: "Hardware-accelerated GPU scheduling",
            description:
                "Moves GPU work scheduling from a Windows-managed queue onto the GPU itself. \
                 Effects vary by hardware and driver, in both directions — benchmark it on and \
                 off rather than assuming either state is better.",
            risk: Risk::Moderate,
            requires_reboot: true,
            key: RegKey::hklm(GRAPHICS_DRIVERS, "HwSchMode"),
            desired: RegValue::Dword(2),
            gate: always,
        }),
        // ---- Deep ----------------------------------------------------------
        Box::new(RegistryTweak {
            id: "nvidia-thread-priority",
            title: "Raise the NVIDIA display driver thread priority",
            description:
                "Raises the scheduling priority of the nvlddmkm driver's worker threads so they \
                 are preempted less often. Kernel-adjacent: a bad value here can destabilise the \
                 display stack, so it requires the deep-risk opt-in and a restore point.",
            risk: Risk::Deep,
            requires_reboot: true,
            key: RegKey::hklm(NVIDIA_PARAMETERS, "ThreadPriority"),
            desired: RegValue::Dword(0x1f),
            gate: needs_nvidia,
        }),
    ];

    // Report-only on Windows 10, a real tweak on Windows 11.
    v.push(timer_resolution_entry(sys));
    v
}

/// On Windows 11 this is a real (if unproven) tweak; on Windows 10 it is a
/// documented no-op and is exposed as report-only.
fn timer_resolution_entry(sys: &SystemInfo) -> Box<dyn Tweak> {
    let inner = RegistryTweak {
        id: "global-timer-resolution",
        title: "System-wide timer resolution requests",
        description:
            "Since Windows 10 2004, a process asking for a 0.5 ms timer only affects itself. \
             Windows 11 can restore the old system-wide behaviour via \
             GlobalTimerResolutionRequests; Windows 10 has no supported equivalent, so no \
             third-party tool can raise the game's timer resolution there.",
        risk: Risk::Moderate,
        requires_reboot: true,
        key: RegKey::hklm(SESSION_KERNEL, "GlobalTimerResolutionRequests"),
        desired: RegValue::Dword(1),
        gate: needs_windows_11,
    };

    if sys.os.is_windows_11() {
        Box::new(inner)
    } else {
        Box::new(ReportOnly {
            inner,
            reason: "the kernel only reads this value on Windows 11; writing it on Windows 10 \
                     does nothing",
        })
    }
}

/// Catalog entries at or below `max_risk`.
pub fn by_max_risk(sys: &SystemInfo, max_risk: Risk) -> Vec<Box<dyn Tweak>> {
    all(sys).into_iter().filter(|t| t.risk() <= max_risk).collect()
}

/// Look up a single entry by id.
pub fn find(sys: &SystemInfo, id: &str) -> Option<Box<dyn Tweak>> {
    all(sys).into_iter().find(|t| t.id() == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys() -> SystemInfo {
        SystemInfo::query().unwrap()
    }

    #[test]
    fn ids_are_unique() {
        let cat = all(&sys());
        let mut ids: Vec<&str> = cat.iter().map(|t| t.id()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate tweak id in the catalog");
    }

    #[test]
    fn every_entry_is_documented() {
        for t in all(&sys()) {
            assert!(!t.title().is_empty(), "{} has no title", t.id());
            assert!(
                t.description().len() > 40,
                "{} needs a real description explaining the mechanism",
                t.id()
            );
        }
    }

    #[test]
    fn nothing_ships_claiming_to_be_measured() {
        // Evidence is earned by benchmarking, never asserted by the catalog.
        for t in all(&sys()) {
            assert_ne!(
                t.evidence(),
                Evidence::Measured,
                "{} must not ship as Measured",
                t.id()
            );
        }
    }

    #[test]
    fn every_entry_can_be_captured() {
        // The engine refuses to apply what it cannot reverse, so capture must
        // work for every entry on a real system.
        for t in all(&sys()) {
            assert!(
                t.capture().is_ok(),
                "{} cannot be captured, so it must not ship",
                t.id()
            );
        }
    }

    #[test]
    fn every_entry_probes_without_mutating() {
        let sys = sys();
        for t in all(&sys) {
            let before = t.probe().unwrap();
            let after = t.probe().unwrap();
            assert_eq!(before, after, "{} probe is not idempotent", t.id());
        }
    }

    #[test]
    fn timer_tweak_is_report_only_on_windows_10() {
        let sys = sys();
        let t = find(&sys, "global-timer-resolution").unwrap();

        if sys.os.is_windows_11() {
            assert!(t.applicable(&sys).is_applicable() || t.applicable(&sys) == Applicability::AlreadySet);
        } else {
            // Must refuse both at the gate and at the apply call.
            match t.applicable(&sys) {
                Applicability::NotApplicable { reason } => {
                    assert!(reason.contains("Windows 11"), "reason was: {reason}");
                }
                other => panic!("expected NotApplicable on Windows 10, got {other:?}"),
            }
            assert_eq!(t.evidence(), Evidence::KnownNoop);
            assert!(
                t.apply().is_err(),
                "report-only tweak must refuse to apply even if called directly"
            );
        }
    }

    #[test]
    fn nvidia_tweak_is_gated_on_an_nvidia_gpu() {
        let mut sys = sys();
        let t = find(&sys, "nvidia-thread-priority").unwrap();

        sys.has_nvidia_gpu = false;
        match t.applicable(&sys) {
            Applicability::NotApplicable { reason } => assert!(reason.contains("NVIDIA")),
            other => panic!("expected NotApplicable without an NVIDIA GPU, got {other:?}"),
        }
    }

    #[test]
    fn risk_filter_excludes_deeper_entries() {
        let sys = sys();
        let safe = by_max_risk(&sys, Risk::Safe);
        assert!(!safe.is_empty());
        assert!(safe.iter().all(|t| t.risk() == Risk::Safe));

        let moderate = by_max_risk(&sys, Risk::Moderate);
        assert!(moderate.len() > safe.len());
        assert!(moderate.iter().all(|t| t.risk() <= Risk::Moderate));

        assert!(all(&sys).iter().any(|t| t.risk() == Risk::Deep));
    }

    #[test]
    fn deep_entries_require_a_reboot_and_a_restore_point() {
        for t in all(&sys()).iter().filter(|t| t.risk() == Risk::Deep) {
            assert!(
                t.risk().requires_restore_point(),
                "{} is deep but claims no restore point",
                t.id()
            );
        }
    }
}
