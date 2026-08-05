//! Closing background applications before a session.
//!
//! Justified by measurement rather than folklore: on a CPU-bound machine, the
//! engine's own instrumentation showed the slow tail was entirely CPU-side, and
//! the desktop was carrying ~30–50% background load on four cores. Reclaiming
//! that is a real lever, unlike most registry tweaks.
//!
//! It is also the most destructive thing OPTEA does, so:
//!
//! * **Allowlist only.** A process is a candidate solely because it appears in
//!   [`CATALOG`]. There is no heuristic that could sweep up something unknown.
//! * **Two tiers.** [`Tier::Auto`] holds background services with no user state
//!   to lose. [`Tier::Ask`] holds anything a person might be mid-way through, so
//!   the decision stays theirs.
//! * **Graceful only.** OPTEA posts the same close request as clicking the X.
//!   An app that declines keeps running.
//! * Nothing is restarted automatically afterwards; the report says what was
//!   closed so it can be reopened deliberately.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Tier {
    /// Background software with nothing for a user to lose.
    Auto,
    /// Holds state, a conversation, or work in progress. Always asks.
    Ask,
}

pub struct KnownApp {
    /// Executable name, without `.exe`.
    pub process: &'static str,
    pub label: &'static str,
    pub tier: Tier,
    /// Why closing it helps.
    pub why: &'static str,
    /// What the user gives up. Shown before asking.
    pub cost: Option<&'static str>,
}

/// Applications OPTEA is willing to close, and nothing else.
pub const CATALOG: &[KnownApp] = &[
    // ---- Auto: launchers, updaters, and decoration --------------------------
    KnownApp {
        process: "EpicGamesLauncher",
        label: "Epic Games Launcher",
        tier: Tier::Auto,
        why: "Polls continuously in the background and is not needed to play a game from \
              another launcher.",
        cost: None,
    },
    KnownApp {
        process: "EpicWebHelper",
        label: "Epic web helper",
        tier: Tier::Auto,
        why: "Helper process for the Epic launcher.",
        cost: None,
    },
    KnownApp {
        process: "wallpaper32",
        label: "Wallpaper Engine",
        tier: Tier::Auto,
        why: "Renders an animated wallpaper that is completely hidden behind a fullscreen game.",
        cost: None,
    },
    KnownApp {
        process: "wallpaper64",
        label: "Wallpaper Engine (64-bit)",
        tier: Tier::Auto,
        why: "Renders an animated wallpaper that is completely hidden behind a fullscreen game.",
        cost: None,
    },
    KnownApp {
        process: "NGenuity2Helper",
        label: "HyperX NGenuity helper",
        tier: Tier::Auto,
        why: "Peripheral configuration software; only needed while changing device settings.",
        cost: Some("Per-device lighting or profile changes cannot be made until reopened."),
    },
    KnownApp {
        process: "iCUE",
        label: "Corsair iCUE",
        tier: Tier::Auto,
        why: "Peripheral configuration and RGB software.",
        cost: Some("RGB effects revert to hardware defaults."),
    },
    KnownApp {
        process: "LogiOverlay",
        label: "Logitech overlay",
        tier: Tier::Auto,
        why: "Peripheral overlay software.",
        cost: None,
    },
    KnownApp {
        process: "GalaxyClient",
        label: "GOG Galaxy",
        tier: Tier::Auto,
        why: "Game launcher polling in the background.",
        cost: None,
    },
    KnownApp {
        process: "Docker Desktop",
        label: "Docker Desktop",
        tier: Tier::Auto,
        why: "Runs a Linux virtual machine and its background services.",
        cost: Some("Running containers stop. Close it yourself if any are doing work."),
    },
    KnownApp {
        process: "com.docker.backend",
        label: "Docker backend",
        tier: Tier::Auto,
        why: "The Docker virtual machine backend.",
        cost: Some("Running containers stop."),
    },
    KnownApp {
        process: "OneDrive",
        label: "OneDrive",
        tier: Tier::Auto,
        why: "Syncs files in the background, competing for CPU and disk.",
        cost: Some("File syncing pauses until reopened."),
    },
    // ---- Ask: things a person may be using ----------------------------------
    KnownApp {
        process: "Discord",
        label: "Discord",
        tier: Tier::Ask,
        why: "Runs several processes and renders an overlay.",
        cost: Some("Voice chat drops. Keep it if you play with a team — a benchmark without \
                    it would not reflect how you actually play."),
    },
    KnownApp {
        process: "steam",
        label: "Steam",
        tier: Tier::Ask,
        why: "Background client and downloads.",
        cost: Some("Only close this if the game was not launched through Steam."),
    },
    KnownApp {
        process: "steamwebhelper",
        label: "Steam web helper",
        tier: Tier::Ask,
        why: "Steam's embedded browser, which is heavier than the client itself.",
        cost: Some("Closing it may restart or close Steam."),
    },
    KnownApp {
        process: "chrome",
        label: "Google Chrome",
        tier: Tier::Ask,
        why: "Browser tabs continue running scripts and timers while you play.",
        cost: Some("Open tabs close. Chrome usually restores them on reopen, but unsaved \
                    form input is lost."),
    },
    KnownApp {
        process: "firefox",
        label: "Firefox",
        tier: Tier::Ask,
        why: "Browser tabs continue running scripts and timers while you play.",
        cost: Some("Open tabs close."),
    },
    KnownApp {
        process: "msedge",
        label: "Microsoft Edge",
        tier: Tier::Ask,
        why: "Browser tabs continue running scripts and timers while you play.",
        cost: Some("Open tabs close."),
    },
    KnownApp {
        process: "Spotify",
        label: "Spotify",
        tier: Tier::Ask,
        why: "Audio decoding and its embedded browser.",
        cost: Some("Music stops."),
    },
    KnownApp {
        process: "obs64",
        label: "OBS Studio",
        tier: Tier::Ask,
        why: "Encoding uses substantial CPU or GPU.",
        cost: Some("Any recording or stream ends. Never close this mid-broadcast."),
    },
    KnownApp {
        process: "Code",
        label: "Visual Studio Code",
        tier: Tier::Ask,
        why: "Language servers and extensions run continuously.",
        cost: Some("Unsaved edits are at risk; it will prompt you to save."),
    },
];

pub fn lookup(process: &str) -> Option<&'static KnownApp> {
    CATALOG
        .iter()
        .find(|a| a.process.eq_ignore_ascii_case(process))
}

/// A catalog entry that is actually running.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub process: String,
    pub label: String,
    pub tier: Tier,
    pub why: String,
    pub cost: Option<String>,
    /// Every pid for this executable; some apps run several.
    pub pids: Vec<u32>,
}

impl Candidate {
    pub fn instance_note(&self) -> String {
        match self.pids.len() {
            1 => String::new(),
            n => format!(" ({n} processes)"),
        }
    }
}

/// Catalog entries currently running, `Auto` first.
pub fn candidates() -> Vec<Candidate> {
    let Ok(running) = optea_sys::process::enumerate() else {
        return Vec::new();
    };

    let mut out: Vec<Candidate> = CATALOG
        .iter()
        .filter_map(|app| {
            // The denylist wins even over an explicit catalog entry, so a bad
            // entry can never reach a protected process.
            if optea_sys::process::is_protected(app.process) {
                return None;
            }
            let pids: Vec<u32> = running
                .iter()
                .filter(|p| p.name.eq_ignore_ascii_case(app.process))
                .map(|p| p.pid)
                .collect();
            if pids.is_empty() {
                return None;
            }
            Some(Candidate {
                process: app.process.to_string(),
                label: app.label.to_string(),
                tier: app.tier,
                why: app.why.to_string(),
                cost: app.cost.map(str::to_owned),
                pids,
            })
        })
        .collect();

    out.sort_by_key(|c| match c.tier {
        Tier::Auto => 0,
        Tier::Ask => 1,
    });
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct CloseResult {
    pub label: String,
    pub process: String,
    pub closed: usize,
    pub still_running: usize,
    pub protected: usize,
}

impl CloseResult {
    pub fn fully_closed(&self) -> bool {
        self.still_running == 0 && self.protected == 0 && self.closed > 0
    }
}

/// Grace period allowed for an app to shut down after being asked.
pub const GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Ask one candidate's processes to close.
pub fn close(candidate: &Candidate) -> CloseResult {
    use optea_sys::process::CloseOutcome;

    let mut closed = 0;
    let mut still_running = 0;
    let mut protected = 0;

    for pid in &candidate.pids {
        match optea_sys::process::close_gracefully(*pid, &candidate.process, GRACE) {
            CloseOutcome::Closed | CloseOutcome::NotFound => closed += 1,
            CloseOutcome::StillRunning => still_running += 1,
            CloseOutcome::Protected => protected += 1,
        }
    }

    CloseResult {
        label: candidate.label.clone(),
        process: candidate.process.clone(),
        closed,
        still_running,
        protected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entries_are_unique() {
        let mut names: Vec<&str> = CATALOG.iter().map(|a| a.process).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate process in CATALOG");
    }

    #[test]
    fn no_catalog_entry_is_a_protected_process() {
        // The two lists must never overlap, or a catalog entry would silently
        // never work — or worse, suggest something dangerous to the user.
        for app in CATALOG {
            assert!(
                !optea_sys::process::is_protected(app.process),
                "{} is both catalogued and protected",
                app.process
            );
        }
    }

    #[test]
    fn anticheat_and_game_are_absent_from_the_catalog() {
        for banned in ["BEService", "BEDaisy", "RainbowSix", "RainbowSix_BE", "explorer"] {
            assert!(
                lookup(banned).is_none(),
                "{banned} must never be a close candidate"
            );
        }
    }

    #[test]
    fn every_entry_explains_itself() {
        for app in CATALOG {
            assert!(!app.label.is_empty(), "{} has no label", app.process);
            assert!(
                app.why.len() > 20,
                "{} needs a real explanation of why closing helps",
                app.process
            );
        }
    }

    #[test]
    fn everything_that_can_lose_work_is_ask_tier() {
        // An app holding a conversation, a stream, or unsaved edits must never
        // be closed without asking.
        for name in ["Discord", "obs64", "Code", "chrome", "firefox", "Spotify"] {
            let app = lookup(name).expect(name);
            assert_eq!(app.tier, Tier::Ask, "{name} must be Ask tier");
        }
    }

    #[test]
    fn ask_tier_entries_state_the_cost() {
        for app in CATALOG.iter().filter(|a| a.tier == Tier::Ask) {
            assert!(
                app.cost.is_some(),
                "{} asks the user but does not say what they lose",
                app.process
            );
        }
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup("discord").is_some());
        assert!(lookup("DISCORD").is_some());
        assert!(lookup("EpicGamesLauncher").is_some());
        assert!(lookup("NotARealApp").is_none());
    }

    #[test]
    fn candidates_put_auto_tier_first() {
        // Only entries actually running appear, so this asserts ordering rather
        // than any particular app being present.
        let c = candidates();
        let first_ask = c.iter().position(|x| x.tier == Tier::Ask);
        let last_auto = c.iter().rposition(|x| x.tier == Tier::Auto);
        if let (Some(fa), Some(la)) = (first_ask, last_auto) {
            assert!(la < fa, "Auto entries must sort before Ask entries");
        }
    }

    #[test]
    fn candidates_never_include_a_protected_process() {
        for c in candidates() {
            assert!(!optea_sys::process::is_protected(&c.process));
        }
    }
}
