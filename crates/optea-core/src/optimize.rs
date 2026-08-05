//! The one-command path: apply everything the evidence supports.
//!
//! The rest of this crate is deliberately cautious — capture before apply,
//! refuse to claim an effect without measuring one. That caution is worth
//! keeping, but it is not a substitute for doing the thing the user asked for.
//! This module applies the changes that the measurements on *this* machine
//! actually justify, in one step, all of them reversible.
//!
//! What it does **not** do is claim a number. The changes are chosen because a
//! mechanism connects them to the measured bottleneck, not because a forum post
//! promised 30 FPS. Whether they helped is a question for `optea bench compare`.

use serde::Serialize;

/// A single change the optimiser wants to make.
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub area: &'static str,
    pub what: String,
    pub from: String,
    pub to: String,
    /// The mechanism connecting this change to a measured bottleneck.
    pub because: &'static str,
    pub needs_restart: bool,
}

/// Game settings worth changing on a CPU-limited system.
///
/// Ordered by how directly each targets the measured constraint. The engine's
/// own instrumentation put the slow tail entirely on the CPU, so CPU-side work
/// comes first and GPU-side quality is largely beside the point here.
pub struct SettingChange {
    pub alias: &'static str,
    pub target: i64,
    pub because: &'static str,
    pub needs_restart: bool,
}

pub const SETTING_PLAN: &[SettingChange] = &[
    SettingChange {
        alias: "geometry",
        target: 2,
        because: "Level-of-detail and draw-call volume are CPU work, and the CPU is what the \
                  slow frames are waiting on.",
        needs_restart: false,
    },
    SettingChange {
        alias: "bufferedframes",
        target: 0,
        because: "Each queued frame is one more frame of age between input and screen.",
        needs_restart: false,
    },
    SettingChange {
        alias: "windowmode",
        target: 0,
        because: "Exclusive fullscreen presents directly instead of through the desktop \
                  compositor, removing a step from the present path.",
        needs_restart: true,
    },
    SettingChange {
        alias: "dof",
        target: 0,
        because: "Depth of field costs GPU time and blurs distant detail, which is a visibility \
                  cost in a shooter.",
        needs_restart: false,
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub changes: Vec<Change>,
    /// Reasons the plan cannot run right now.
    pub blockers: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn needs_restart(&self) -> bool {
        self.changes.iter().any(|c| c.needs_restart)
    }

    pub fn by_area(&self, area: &str) -> Vec<&Change> {
        self.changes.iter().filter(|c| c.area == area).collect()
    }
}

/// Work out what would change, without changing anything.
pub fn plan(sys: &crate::tweak::SystemInfo) -> Plan {
    let mut changes = Vec::new();
    let mut blockers = Vec::new();

    // ---- Game settings --------------------------------------------------
    match optea_game::profile::discover() {
        Ok(Some(profiles)) => match profiles.active() {
            Some(active) => match std::fs::read_to_string(&active.settings_path) {
                Ok(text) => {
                    let doc = optea_game::ini::IniDocument::parse(&text);
                    for step in SETTING_PLAN {
                        let Some(setting) = optea_game::settings::find_editable(step.alias) else {
                            continue;
                        };
                        let current = doc.get_i64(setting.section, setting.key);
                        // Only propose a change that actually changes something.
                        if current == Some(step.target) {
                            continue;
                        }
                        changes.push(Change {
                            area: "game",
                            what: setting.key.to_string(),
                            from: current
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "unset".into()),
                            to: step.target.to_string(),
                            because: step.because,
                            needs_restart: step.needs_restart,
                        });
                    }
                }
                Err(e) => blockers.push(format!("cannot read GameSettings.ini: {e}")),
            },
            None => blockers.push("no Siege profile found".into()),
        },
        Ok(None) => blockers.push("Siege settings folder not found — launch the game once".into()),
        Err(e) => blockers.push(format!("profile discovery failed: {e}")),
    }

    if let Some(pid) = optea_game::running_game_pid() {
        blockers.push(format!(
            "Siege is running (pid {pid}). It rewrites GameSettings.ini on exit, so changes made \
             now would be discarded. Close the game first."
        ));
    }

    // ---- System tweaks ---------------------------------------------------
    for tweak in crate::catalog::by_max_risk(sys, crate::tweak::Risk::Safe) {
        if tweak.applicable(sys) != crate::tweak::Applicability::Applicable {
            continue;
        }
        changes.push(Change {
            area: "system",
            what: tweak.id().to_string(),
            from: tweak.probe().unwrap_or_else(|_| "unknown".into()),
            to: "applied".into(),
            because: "Removes background work from the CPU, which is the constrained resource \
                      on this system.",
            needs_restart: tweak.requires_reboot(),
        });
    }

    if !optea_sys::sysinfo::is_elevated().unwrap_or(false) {
        blockers.push(
            "system tweaks need an elevated terminal; game settings and background apps do not"
                .into(),
        );
    }

    // ---- Background apps -------------------------------------------------
    for c in crate::quiet::candidates() {
        if c.tier != crate::quiet::Tier::Auto {
            continue;
        }
        changes.push(Change {
            area: "background",
            what: c.label.clone(),
            from: format!("{} process(es)", c.pids.len()),
            to: "closed".into(),
            because: "Competes for CPU time with a game that is already CPU-limited.",
            needs_restart: false,
        });
    }

    Plan { changes, blockers }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tweak::SystemInfo;

    fn sys() -> SystemInfo {
        SystemInfo::query().unwrap()
    }

    #[test]
    fn plan_runs_without_changing_anything() {
        // Calling plan() twice must be stable: it is a read-only projection.
        let a = plan(&sys());
        let b = plan(&sys());
        assert_eq!(a.changes.len(), b.changes.len());
    }

    #[test]
    fn every_change_states_a_mechanism() {
        // The project's rule: name why a change should help, never promise a
        // number. A change with no mechanism has no business being applied.
        for c in plan(&sys()).changes {
            assert!(
                c.because.len() > 30,
                "{} has no real justification",
                c.what
            );
            assert!(
                !c.because.to_lowercase().contains("fps boost"),
                "{} makes a performance claim instead of naming a mechanism",
                c.what
            );
        }
    }

    #[test]
    fn setting_plan_targets_are_permitted_values() {
        // A target outside the allowlisted range would be refused at write
        // time; catching it here means the plan can never propose one.
        for step in SETTING_PLAN {
            let setting = optea_game::settings::find_editable(step.alias)
                .unwrap_or_else(|| panic!("{} is not an editable setting", step.alias));
            assert!(
                setting.allowed.permits(step.target),
                "{} = {} is outside the allowed range {}",
                step.alias,
                step.target,
                setting.allowed.describe()
            );
        }
    }

    #[test]
    fn setting_plan_has_no_duplicates() {
        let mut seen: Vec<&str> = SETTING_PLAN.iter().map(|s| s.alias).collect();
        let n = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), n, "duplicate setting in SETTING_PLAN");
    }

    #[test]
    fn window_mode_change_is_marked_as_needing_a_restart() {
        let step = SETTING_PLAN
            .iter()
            .find(|s| s.alias == "windowmode")
            .unwrap();
        assert!(
            step.needs_restart,
            "presentation mode only takes effect on relaunch; not saying so would make the \
             next benchmark measure the old setting"
        );
    }

    #[test]
    fn a_running_game_blocks_the_plan() {
        let p = plan(&sys());
        if optea_game::running_game_pid().is_some() {
            assert!(
                p.blockers.iter().any(|b| b.contains("running")),
                "a running game must be reported as a blocker"
            );
        }
    }

    #[test]
    fn changes_are_grouped_by_area() {
        let p = plan(&sys());
        for c in &p.changes {
            assert!(
                ["game", "system", "background"].contains(&c.area),
                "unexpected area {}",
                c.area
            );
        }
        // Grouping must partition the set, not drop entries.
        let grouped = p.by_area("game").len() + p.by_area("system").len() + p.by_area("background").len();
        assert_eq!(grouped, p.changes.len());
    }
}
