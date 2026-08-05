//! Foreground-window tracking.
//!
//! Games commonly throttle or suspend rendering when they lose focus — Siege
//! drops to roughly 30 FPS. A capture taken while the game is in the background
//! therefore produces numbers that look entirely plausible but describe the
//! throttle rather than the game. Comparing two such runs would attribute the
//! difference to whatever tweak was being tested.
//!
//! This is worse than a capture that obviously fails, so focus is sampled
//! throughout a capture rather than assumed.

use crate::error::{Result, SysError};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// PID owning the foreground window, if there is one.
pub fn foreground_pid() -> Option<u32> {
    let hwnd: HWND = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

pub fn is_foreground(pid: u32) -> bool {
    foreground_pid() == Some(pid)
}

/// Tracks how much of a capture window a process actually held focus for.
#[derive(Debug, Clone, Default)]
pub struct FocusMonitor {
    target: u32,
    samples: u32,
    focused: u32,
}

impl FocusMonitor {
    pub fn new(target_pid: u32) -> Self {
        FocusMonitor {
            target: target_pid,
            ..Default::default()
        }
    }

    /// Take one focus reading. Call periodically during a capture.
    pub fn sample(&mut self) {
        self.samples += 1;
        if is_foreground(self.target) {
            self.focused += 1;
        }
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Fraction of samples where the target held focus, 0.0 to 1.0.
    pub fn focused_fraction(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        self.focused as f64 / self.samples as f64
    }

    /// True when the target held focus for essentially the whole capture.
    ///
    /// A small tolerance allows for the moment focus is handed over at the
    /// start of a run without failing an otherwise clean capture.
    pub fn was_focused_throughout(&self) -> bool {
        self.samples > 0 && self.focused_fraction() >= 0.95
    }

    /// Human-readable verdict for a capture report.
    pub fn describe(&self) -> String {
        if self.samples == 0 {
            return "focus was not sampled".into();
        }
        let pct = self.focused_fraction() * 100.0;
        if self.was_focused_throughout() {
            format!("game held focus for {pct:.0}% of the capture")
        } else {
            format!(
                "game held focus for only {pct:.0}% of the capture — most games throttle \
                 rendering in the background, so these numbers describe the throttle, not the game"
            )
        }
    }
}

/// Confirm a process is in the foreground, as a hard precondition.
pub fn require_foreground(pid: u32) -> Result<()> {
    match foreground_pid() {
        Some(p) if p == pid => Ok(()),
        Some(other) => Err(SysError::msg(format!(
            "process {pid} is not in the foreground (pid {other} is). Focus the game window \
             before capturing."
        ))),
        None => Err(SysError::msg(
            "no foreground window could be determined".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_query_does_not_panic() {
        let _ = foreground_pid();
    }

    #[test]
    fn a_process_that_is_not_focused_is_reported_as_such() {
        // PID 0 is never a foreground window.
        assert!(!is_foreground(0));
        assert!(require_foreground(0).is_err());
    }

    #[test]
    fn monitor_with_no_samples_is_not_considered_focused() {
        let m = FocusMonitor::new(1234);
        assert_eq!(m.focused_fraction(), 0.0);
        assert!(!m.was_focused_throughout());
        assert!(m.describe().contains("not sampled"));
    }

    #[test]
    fn monitor_counts_unfocused_samples() {
        // Sampling a PID that is definitely not focused must yield 0%.
        let mut m = FocusMonitor::new(0);
        for _ in 0..10 {
            m.sample();
        }
        assert_eq!(m.samples(), 10);
        assert_eq!(m.focused_fraction(), 0.0);
        assert!(!m.was_focused_throughout());
        assert!(
            m.describe().contains("throttle"),
            "an unfocused capture must explain why the numbers are wrong: {}",
            m.describe()
        );
    }

    #[test]
    fn threshold_tolerates_a_brief_focus_change() {
        let mut m = FocusMonitor::new(999_999);
        // Simulate 96% focused by writing the counters directly.
        m.samples = 100;
        m.focused = 96;
        assert!(m.was_focused_throughout());

        m.focused = 80;
        assert!(!m.was_focused_throughout(), "80% must not pass as clean");
    }
}
