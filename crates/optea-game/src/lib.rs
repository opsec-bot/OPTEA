//! Rainbow Six Siege specifics: GameSettings.ini, profile discovery, launch chain.

pub mod backup;
pub mod profile;

pub use backup::{Backup, BackupStore, GuardedFile};

/// Executables that present frames, in priority order.
///
/// `RainbowSix_BE.exe` is excluded deliberately: it is the BattlEye launcher
/// shim, which has no window and never presents.
pub const GAME_EXECUTABLES: &[&str] = &["RainbowSix", "RainbowSixGame"];

/// Processes that mean "Siege is open", including the launcher shim.
///
/// Broader than [`GAME_EXECUTABLES`] on purpose: for deciding whether it is safe
/// to edit `GameSettings.ini`, the shim being alive still means the game is
/// mid-session and will rewrite the file on exit.
const SESSION_EXECUTABLES: &[&str] = &["RainbowSix", "RainbowSixGame", "RainbowSix_BE"];

/// PID of a running Siege process, if any.
pub fn running_game_pid() -> Option<u32> {
    let out = std::process::Command::new("tasklist")
        .args(["/fo", "csv", "/nh"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);

    for line in text.lines() {
        let fields: Vec<&str> = line.split("\",\"").collect();
        if fields.len() < 2 {
            continue;
        }
        let name = fields[0].trim_start_matches('"');
        let stem = name.strip_suffix(".exe").unwrap_or(name);
        if SESSION_EXECUTABLES
            .iter()
            .any(|g| g.eq_ignore_ascii_case(stem))
        {
            if let Ok(pid) = fields[1].trim().parse() {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_shim_counts_as_a_session_but_not_a_capture_target() {
        // Capturing from the shim yields zero frames, but its presence still
        // means an edit would be clobbered on exit.
        assert!(!GAME_EXECUTABLES.contains(&"RainbowSix_BE"));
        assert!(SESSION_EXECUTABLES.contains(&"RainbowSix_BE"));
    }

    #[test]
    fn game_detection_does_not_error() {
        // Returns Some or None depending on whether Siege is open; must not panic.
        let _ = running_game_pid();
    }
}
