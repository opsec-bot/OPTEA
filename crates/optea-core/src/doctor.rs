//! Read-only system diagnostics.
//!
//! `doctor` runs before any tweak exists and is deliberately willing to report
//! that nothing is wrong, or that the thing limiting the machine is hardware
//! rather than configuration. An optimizer that always finds something to fix is
//! an optimizer that is making things up.

use optea_sys::display::DisplayInfo;
use optea_sys::gpu::GpuDevice;
use optea_sys::power::PowerState;
use optea_sys::registry::{self, RegKey, RegValue};
use optea_sys::sysinfo::{self, CpuInfo, OsInfo};
use serde::Serialize;

/// Physical cores below which Siege is meaningfully CPU-limited.
const SIEGE_COMFORTABLE_CORES: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Severity {
    /// Confirmed already in the desired state. Worth saying out loud so the user
    /// does not "fix" it again with some script off the internet.
    Good,
    Info,
    Warn,
    Critical,
}

impl Severity {
    pub fn label(&self) -> &'static str {
        match self {
            Severity::Good => "OK",
            Severity::Info => "INFO",
            Severity::Warn => "WARN",
            Severity::Critical => "CRIT",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// What to do about it, when there is something to do.
    pub advice: Option<String>,
}

impl Finding {
    fn new(severity: Severity, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Finding {
            severity,
            title: title.into(),
            detail: detail.into(),
            advice: None,
        }
    }

    fn with_advice(mut self, advice: impl Into<String>) -> Self {
        self.advice = Some(advice.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistryProbe {
    pub label: &'static str,
    pub path: String,
    pub value: Option<RegValue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SiegeSummary {
    pub root: String,
    pub profile_count: usize,
    pub active_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub os: OsInfo,
    pub cpu: CpuInfo,
    pub elevated: bool,
    pub power: Option<PowerState>,
    pub displays: Vec<DisplayInfo>,
    pub gpus: Vec<GpuDevice>,
    pub registry: Vec<RegistryProbe>,
    pub siege: Option<SiegeSummary>,
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn worst_severity(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).max()
    }
}

fn probe(label: &'static str, key: RegKey) -> RegistryProbe {
    RegistryProbe {
        label,
        path: format!("{}\\{}", key.display_path(), key.name),
        value: registry::read(&key).ok().flatten(),
    }
}

/// Collect a full diagnostic report. Never mutates system state.
pub fn run() -> anyhow::Result<Report> {
    let os = OsInfo::query()?;
    let cpu = CpuInfo::query()?;
    let elevated = sysinfo::is_elevated().unwrap_or(false);
    let power = PowerState::query().ok();
    let displays = optea_sys::display::enumerate().unwrap_or_default();
    let gpus = optea_sys::gpu::enumerate().unwrap_or_default();

    let registry = vec![
        probe(
            "Win32PrioritySeparation",
            RegKey::hklm(
                r"SYSTEM\CurrentControlSet\Control\PriorityControl",
                "Win32PrioritySeparation",
            ),
        ),
        probe(
            "HwSchMode (HAGS)",
            RegKey::hklm(
                r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers",
                "HwSchMode",
            ),
        ),
        probe(
            "GlobalTimerResolutionRequests",
            RegKey::hklm(
                r"SYSTEM\CurrentControlSet\Control\Session Manager\kernel",
                "GlobalTimerResolutionRequests",
            ),
        ),
    ];

    let siege = optea_game::profile::discover().ok().flatten().map(|p| SiegeSummary {
        root: p.root.display().to_string(),
        profile_count: p.profiles.len(),
        active_profile: p.active().map(|a| a.id.clone()),
    });

    let mut findings = Vec::new();
    check_display_routing(&displays, &gpus, &mut findings);
    check_gpu_health(&gpus, &displays, &mut findings);
    check_cpu_headroom(&cpu, &displays, &mut findings);
    check_power(power.as_ref(), &mut findings);
    check_timer_resolution(&os, &mut findings);
    check_siege(siege.as_ref(), &mut findings);
    check_measurement(&mut findings);
    check_elevation(elevated, &mut findings);

    findings.sort_by(|a, b| b.severity.cmp(&a.severity));

    Ok(Report {
        os,
        cpu,
        elevated,
        power,
        displays,
        gpus,
        registry,
        siege,
        findings,
    })
}

/// The highest-value check in the whole tool: a display plugged into the
/// motherboard routes every frame through integrated graphics.
fn check_display_routing(
    displays: &[DisplayInfo],
    gpus: &[GpuDevice],
    out: &mut Vec<Finding>,
) {
    if displays.is_empty() {
        return;
    }

    let has_discrete = gpus.iter().any(|g| !g.looks_integrated());
    let on_integrated: Vec<&DisplayInfo> = displays
        .iter()
        .filter(|d| {
            let n = d.gpu_name.to_lowercase();
            n.contains("vega")
                || n.contains("intel(r) hd")
                || n.contains("intel(r) uhd")
                || n.contains("intel(r) iris")
        })
        .collect();

    if has_discrete && !on_integrated.is_empty() {
        let names: Vec<String> = on_integrated
            .iter()
            .map(|d| format!("{} ({})", d.gdi_name, d.gpu_name))
            .collect();
        out.push(
            Finding::new(
                Severity::Critical,
                "Display connected to integrated graphics",
                format!(
                    "These outputs are driven by the iGPU while a discrete GPU is present: {}",
                    names.join(", ")
                ),
            )
            .with_advice(
                "Move the cable to a port on the discrete graphics card. This costs more FPS \
                 and latency than every software tweak in this tool combined.",
            ),
        );
    } else if has_discrete {
        let gpu = &displays[0].gpu_name;
        out.push(Finding::new(
            Severity::Good,
            "Displays driven by the discrete GPU",
            format!("All active outputs are on {gpu}."),
        ));
    }

    if displays.len() > 1 {
        let mut rates: Vec<i64> = displays.iter().map(|d| d.refresh_hz.round() as i64).collect();
        rates.sort_unstable();
        rates.dedup();

        if rates.len() > 1 {
            // Mixed rates can pin the compositor to a lowest common denominator
            // on some driver versions.
            out.push(
                Finding::new(
                    Severity::Info,
                    "Mixed refresh rates across displays",
                    format!(
                        "Active displays run at {} Hz.",
                        rates
                            .iter()
                            .map(|r| r.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
                .with_advice(
                    "Worth benchmarking with the secondary display disabled — the effect is \
                     driver- and version-dependent, so measure rather than assume.",
                ),
            );
        } else {
            // Even at matched rates, additional high-refresh surfaces cost
            // compositor work and VRAM, which bites hardest on small frame buffers.
            out.push(
                Finding::new(
                    Severity::Info,
                    format!("{} active displays", displays.len()),
                    format!(
                        "All running {} Hz. Additional displays cost desktop-compositor work \
                         and video memory even while the game is fullscreen.",
                        rates.first().copied().unwrap_or_default()
                    ),
                )
                .with_advice(
                    "A worthwhile A/B: benchmark with secondary displays disabled. Treat any \
                     difference as measured, not assumed.",
                ),
            );
        }
    }
}

fn check_gpu_health(gpus: &[GpuDevice], displays: &[DisplayInfo], out: &mut Vec<Finding>) {
    for gpu in gpus.iter().filter(|g| !g.is_healthy()) {
        let problem = gpu.problem_text().unwrap_or("unknown problem");
        // A faulted iGPU is cosmetic when a healthy discrete GPU drives every
        // display; saying so prevents a pointless driver-reinstall rabbit hole.
        let drives_a_display = displays.iter().any(|d| d.gpu_name == gpu.name);
        let (sev, advice) = if !drives_a_display && gpu.looks_integrated() {
            (
                Severity::Info,
                "Harmless while the discrete GPU drives all displays. No action needed.",
            )
        } else {
            (
                Severity::Warn,
                "This adapter is faulted and is or may be needed. Reinstall or re-enable its driver.",
            )
        };
        out.push(
            Finding::new(
                sev,
                format!("Display adapter faulted: {}", gpu.name),
                format!("Device reports: {problem}."),
            )
            .with_advice(advice),
        );
    }
}

fn check_cpu_headroom(cpu: &CpuInfo, displays: &[DisplayInfo], out: &mut Vec<Finding>) {
    if cpu.physical_cores >= SIEGE_COMFORTABLE_CORES {
        return;
    }

    let primary = displays.iter().find(|d| d.is_primary).or(displays.first());
    // Deliberately phrased as the *display* mode. The game may well render at a
    // lower resolution than the desktop, so implying the two are the same would
    // misstate the workload — `optea siege settings` reports the render side.
    let mode = primary
        .map(|d| format!(" The display runs at {}.", d.mode_string()))
        .unwrap_or_default();

    out.push(
        Finding::new(
            Severity::Warn,
            "CPU is the likely frame-rate limit",
            format!(
                "{} has {} physical cores / {} threads. Siege is CPU-bound even on modern \
                 high-core-count parts.{mode}",
                cpu.name, cpu.physical_cores, cpu.logical_processors
            ),
        )
        .with_advice(
            "System tweaks realistically move 1% lows by single-digit percent here. Run \
             `optea siege settings` — render resolution and CPU-side quality settings are far \
             larger levers.",
        ),
    );
}

fn check_power(power: Option<&PowerState>, out: &mut Vec<Finding>) {
    let Some(power) = power else { return };

    if power.is_high_performance() {
        out.push(Finding::new(
            Severity::Good,
            "Power plan already favours performance",
            format!("Active scheme: {}.", power.scheme_name),
        ));
    } else {
        out.push(
            Finding::new(
                Severity::Warn,
                "Power plan is not a performance plan",
                format!("Active scheme: {}.", power.scheme_name),
            )
            .with_advice("Switch to High performance or Ultimate Performance."),
        );
    }

    match power.min_cores_percent_ac {
        Some(100) => out.push(Finding::new(
            Severity::Good,
            "Core parking already disabled",
            "Processor minimum cores is 100% on AC.",
        )),
        Some(pct) => out.push(
            Finding::new(
                Severity::Warn,
                "Core parking is active",
                format!("Processor minimum cores is {pct}% on AC."),
            )
            .with_advice("Raise the processor core-parking floor to 100%."),
        ),
        None => {}
    }
}

fn check_timer_resolution(os: &OsInfo, out: &mut Vec<Finding>) {
    if !os.has_per_process_timer_resolution() {
        return;
    }

    if os.supports_global_timer_resolution() {
        out.push(
            Finding::new(
                Severity::Info,
                "Global timer resolution available",
                "Windows 11 honours GlobalTimerResolutionRequests, so a system-wide 0.5 ms \
                 timer can be restored.",
            )
            .with_advice("Benchmark it — availability is not evidence that it helps."),
        );
    } else {
        // Stated plainly because this is one of the most widely repeated tweaks
        // on the internet and it cannot work on this OS.
        out.push(
            Finding::new(
                Severity::Info,
                "Timer resolution tweaks do not apply on this OS",
                format!(
                    "Since Windows 10 2004, timer resolution is per-process. The \
                     GlobalTimerResolutionRequests override exists only on Windows 11 \
                     (build {}+); this system is build {}.",
                    sysinfo::WIN11_MIN_BUILD,
                    os.build
                ),
            )
            .with_advice(
                "No third-party tool can raise the game's timer resolution here. Ignore any \
                 guide that claims otherwise.",
            ),
        );
    }
}

fn check_siege(siege: Option<&SiegeSummary>, out: &mut Vec<Finding>) {
    let Some(siege) = siege else {
        out.push(
            Finding::new(
                Severity::Warn,
                "Siege settings folder not found",
                "No settings were found under Documents\\My Games\\Rainbow Six - Siege.",
            )
            .with_advice("Launch the game once so it writes GameSettings.ini."),
        );
        return;
    };

    if siege.profile_count > 1 {
        out.push(
            Finding::new(
                Severity::Info,
                "Multiple Siege profiles present",
                format!(
                    "{} profile folders found; the most recently written is {}.",
                    siege.profile_count,
                    siege.active_profile.as_deref().unwrap_or("unknown")
                ),
            )
            .with_advice(
                "OPTEA edits the most recently written profile. Launch the game with the \
                 account you play on if that looks wrong.",
            ),
        );
    }
}

fn check_measurement(out: &mut Vec<Finding>) {
    if optea_metrics::presentmon::is_installed() {
        out.push(Finding::new(
            Severity::Good,
            "PresentMon available",
            "Frame capture is available, so tweaks can be A/B benchmarked.",
        ));
    } else {
        out.push(
            Finding::new(
                Severity::Warn,
                "PresentMon not installed — tweaks cannot be verified",
                "Without frame capture, OPTEA can apply changes but cannot tell you whether \
                 any of them actually helped on this machine.",
            )
            .with_advice(
                "Install Intel PresentMon from https://github.com/GameTechDev/PresentMon/releases \
                 (service + SDK), then re-run `optea doctor`.",
            ),
        );
    }
}

fn check_elevation(elevated: bool, out: &mut Vec<Finding>) {
    if !elevated {
        out.push(
            Finding::new(
                Severity::Info,
                "Not running elevated",
                "Diagnostics work unelevated, but applying or reverting tweaks needs \
                 administrator rights.",
            )
            .with_advice("Re-run from an elevated terminal to apply changes."),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_runs_without_elevation() {
        let report = run().expect("doctor must work unelevated");
        assert!(report.os.build > 10000);
        assert!(!report.displays.is_empty());
        assert!(!report.gpus.is_empty());
        assert_eq!(report.registry.len(), 3);
    }

    #[test]
    fn findings_are_sorted_worst_first() {
        let report = run().unwrap();
        let sevs: Vec<Severity> = report.findings.iter().map(|f| f.severity).collect();
        let mut sorted = sevs.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(sevs, sorted, "findings must be ordered most severe first");
    }

    #[test]
    fn timer_finding_matches_os_build() {
        let report = run().unwrap();
        let timer = report
            .findings
            .iter()
            .find(|f| f.title.to_lowercase().contains("timer"))
            .expect("a timer finding should always be produced on 2004+");

        if report.os.is_windows_11() {
            assert!(timer.title.contains("available"));
        } else {
            assert!(
                timer.title.contains("do not apply"),
                "Windows 10 must be told the tweak is inapplicable, got: {}",
                timer.title
            );
        }
    }

    #[test]
    fn every_non_good_finding_offers_advice() {
        // A warning with no remedy is noise.
        for f in run().unwrap().findings {
            if f.severity != Severity::Good {
                assert!(
                    f.advice.is_some(),
                    "finding '{}' has severity {:?} but no advice",
                    f.title,
                    f.severity
                );
            }
        }
    }
}
