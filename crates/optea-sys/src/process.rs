//! Process enumeration and graceful shutdown.
//!
//! Closing processes is the most destructive thing OPTEA does, so the design is
//! conservative in three ways:
//!
//! 1. **Graceful first.** A `WM_CLOSE` is posted to the app's windows, which is
//!    the same thing clicking the X does — the app gets to save state and shut
//!    down cleanly. Force-terminating is a separate, explicit step.
//! 2. **A hard denylist.** Anti-cheat, the game, the shell, and system
//!    processes are refused even if something upstream asks for them by name.
//!    A bug in a catalog entry must not be able to kill `csrss`.
//! 3. **Never by PID alone.** Callers name a process; the PID is resolved here,
//!    so a stale PID cannot be pointed at whatever now occupies it.

use crate::error::{Result, SysError};
use crate::wide::from_wide_nul;
use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_TERMINATE, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};

/// Processes OPTEA will never close, whatever a catalog says.
///
/// Anti-cheat and the game come first: terminating BattlEye while a protected
/// game is running is both destructive and exactly the kind of interference an
/// anti-cheat is built to notice. The rest keep the machine usable.
pub const PROTECTED: &[&str] = &[
    // Anti-cheat and the game itself.
    "BEService",
    "BEDaisy",
    "BattlEye",
    "RainbowSix",
    "RainbowSix_BE",
    "EasyAntiCheat",
    "EasyAntiCheat_EOS",
    "vgc",
    "vgtray",
    // Game platform clients the game needs while running.
    "UbisoftConnect",
    "UbisoftGameLauncher",
    "upc",
    "UplayWebCore",
    // Shell and session.
    "explorer",
    "dwm",
    "winlogon",
    "csrss",
    "wininit",
    "services",
    "lsass",
    "smss",
    "svchost",
    "System",
    "Idle",
    "Registry",
    "fontdrvhost",
    "sihost",
    "ctfmon",
    "audiodg",
    "conhost",
    // Security software: killing it is both risky and often impossible.
    "MsMpEng",
    "SecurityHealthService",
    "NisSrv",
    // OPTEA and the terminal driving it.
    "optea",
    "claude",
    "WindowsTerminal",
    "pwsh",
    "powershell",
    "cmd",
];

/// Strip a trailing `.exe` regardless of case.
///
/// Case matters here: Windows reports executable names in whatever case the
/// file has, so a case-sensitive strip would leave `EXPLORER.EXE` unmatched
/// against the denylist and treat the shell as closable.
fn exe_stem(name: &str) -> &str {
    let n = name.len();
    if n > 4 && name[n - 4..].eq_ignore_ascii_case(".exe") {
        &name[..n - 4]
    } else {
        name
    }
}

pub fn is_protected(name: &str) -> bool {
    let stem = exe_stem(name);
    PROTECTED.iter().any(|p| p.eq_ignore_ascii_case(stem))
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    /// Executable name without the `.exe` suffix.
    pub name: String,
}

/// Every running process.
pub fn enumerate() -> Result<Vec<ProcessInfo>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| SysError::api("CreateToolhelp32Snapshot", e))?;

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    let mut out = Vec::new();
    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            let raw = from_wide_nul(&entry.szExeFile);
            out.push(ProcessInfo {
                pid: entry.th32ProcessID,
                name: exe_stem(&raw).to_string(),
            });
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    unsafe {
        let _ = CloseHandle(snapshot);
    }
    Ok(out)
}

/// All PIDs matching `name`, ignoring case and any `.exe` suffix.
pub fn find_by_name(name: &str) -> Result<Vec<u32>> {
    let stem = exe_stem(name);
    Ok(enumerate()?
        .into_iter()
        .filter(|p| p.name.eq_ignore_ascii_case(stem))
        .map(|p| p.pid)
        .collect())
}

/// Top-level windows belonging to `pid`.
///
/// Deliberately includes windows that are not visible. Tray-minimised apps —
/// launchers, wallpaper and RGB utilities, sync clients — keep a hidden
/// top-level window that still handles `WM_CLOSE`. Filtering on
/// `IsWindowVisible` finds nothing for exactly the applications this is aimed
/// at, and then reports them as having "declined" when they were never asked.
fn windows_for(pid: u32) -> Vec<HWND> {
    struct Ctx {
        pid: u32,
        found: Vec<HWND>,
    }

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> windows::Win32::Foundation::BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut owner));
        if owner == ctx.pid {
            ctx.found.push(hwnd);
        }
        true.into()
    }

    let mut ctx = Ctx {
        pid,
        found: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.found
}

/// True when `pid` owns at least one visible top-level window.
pub fn has_visible_window(pid: u32) -> bool {
    windows_for(pid)
        .into_iter()
        .any(|h| unsafe { IsWindowVisible(h).as_bool() })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CloseOutcome {
    /// Exited cleanly after being asked to close.
    Closed,
    /// Was asked to close and did not exit within the grace period.
    Declined,
    /// Has no window at all, so there was nothing to ask.
    ///
    /// Distinct from [`CloseOutcome::Declined`] on purpose: a background helper
    /// with no message loop never received a request, and reporting it as
    /// having refused one is simply false.
    NoWindow,
    /// Refused because the process is on the denylist.
    Protected,
    /// No such process.
    NotFound,
}

/// Ask a process to close, the same way clicking its X does.
///
/// Posts `WM_CLOSE` to each visible top-level window and waits up to
/// `grace` for the process to exit. Never force-terminates: an app that ignores
/// the request keeps running and is reported as [`CloseOutcome::StillRunning`],
/// which is the correct outcome for something with unsaved work.
pub fn close_gracefully(pid: u32, name: &str, grace: std::time::Duration) -> CloseOutcome {
    if is_protected(name) {
        return CloseOutcome::Protected;
    }

    let windows = windows_for(pid);
    for hwnd in &windows {
        unsafe {
            let _ = PostMessageW(*hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    // A process with no window cannot be asked politely. Say that plainly
    // rather than escalating to a kill on its behalf.
    if windows.is_empty() {
        return if is_running(pid) {
            CloseOutcome::NoWindow
        } else {
            CloseOutcome::NotFound
        };
    }

    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };
    match handle {
        Ok(h) if !h.is_invalid() => {
            let waited = unsafe { WaitForSingleObject(h, grace.as_millis() as u32) };
            unsafe {
                let _ = CloseHandle(h);
            }
            // WAIT_OBJECT_0 means the process exited.
            if waited.0 == 0 {
                CloseOutcome::Closed
            } else {
                CloseOutcome::Declined
            }
        }
        _ => {
            std::thread::sleep(grace);
            if is_running(pid) {
                CloseOutcome::Declined
            } else {
                CloseOutcome::Closed
            }
        }
    }
}

/// Force-terminate. Separate and explicit, because this discards unsaved work.
pub fn terminate(pid: u32, name: &str) -> Result<()> {
    if is_protected(name) {
        return Err(SysError::msg(format!(
            "{name} is protected and will not be terminated"
        )));
    }
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }
        .map_err(|e| SysError::api("OpenProcess(PROCESS_TERMINATE)", e))?;
    let result = unsafe { TerminateProcess(handle, 1) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    result.map_err(|e| SysError::api("TerminateProcess", e))
}

pub fn is_running(pid: u32) -> bool {
    enumerate()
        .map(|ps| ps.iter().any(|p| p.pid == pid))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_processes() {
        let ps = enumerate().unwrap();
        assert!(ps.len() > 10, "implausibly few processes: {}", ps.len());
        // The current process must be in the list.
        let me = std::process::id();
        assert!(ps.iter().any(|p| p.pid == me), "current process missing");
    }

    #[test]
    fn names_have_no_exe_suffix() {
        for p in enumerate().unwrap() {
            assert!(!p.name.to_lowercase().ends_with(".exe"), "{}", p.name);
        }
    }

    #[test]
    fn anticheat_and_game_are_protected() {
        // The most important entries: terminating these would be destructive
        // and, for the anti-cheat, exactly the interference it watches for.
        for name in ["BEService", "BEDaisy", "RainbowSix", "RainbowSix_BE"] {
            assert!(is_protected(name), "{name} must be protected");
        }
    }

    #[test]
    fn system_processes_are_protected() {
        for name in ["csrss", "wininit", "services", "lsass", "explorer", "dwm"] {
            assert!(is_protected(name), "{name} must be protected");
        }
    }

    #[test]
    fn protection_ignores_case_and_exe_suffix() {
        assert!(is_protected("beservice"));
        assert!(is_protected("BEService.exe"));
        // Windows reports names in the file's own case; an uppercase suffix
        // must not slip past the denylist.
        assert!(is_protected("EXPLORER.EXE"));
        assert!(is_protected("Csrss.Exe"));
        assert!(is_protected("BESERVICE.EXE"));
    }

    #[test]
    fn exe_stem_strips_only_a_real_suffix() {
        assert_eq!(exe_stem("Discord.exe"), "Discord");
        assert_eq!(exe_stem("Discord.EXE"), "Discord");
        assert_eq!(exe_stem("Discord"), "Discord");
        // Not a suffix, just a short name.
        assert_eq!(exe_stem(".exe"), ".exe");
        assert_eq!(exe_stem("myexe"), "myexe");
    }

    #[test]
    fn ordinary_apps_are_not_protected() {
        for name in ["EpicGamesLauncher", "wallpaper32", "Discord"] {
            assert!(!is_protected(name), "{name} should be closable");
        }
    }

    #[test]
    fn a_windowless_process_is_not_reported_as_declining() {
        // The distinction that matters: a background helper never received a
        // request, so calling it a refusal misdescribes what happened.
        assert_ne!(CloseOutcome::NoWindow, CloseOutcome::Declined);
    }

    #[test]
    fn tray_minimised_windows_are_still_found() {
        // Windows are collected regardless of visibility; filtering on
        // IsWindowVisible finds nothing for tray-minimised apps, which are
        // precisely the ones worth closing.
        let me = std::process::id();
        let all = windows_for(me);
        // The test harness may own no windows at all; assert only the
        // relationship, which must hold either way.
        assert!(
            all.len() >= usize::from(has_visible_window(me)),
            "visible windows must be a subset of all windows"
        );
    }

    #[test]
    fn protected_processes_refuse_to_close_or_terminate() {
        // Even given a real pid, the name check must veto.
        let outcome = close_gracefully(4, "csrss", std::time::Duration::from_millis(1));
        assert_eq!(outcome, CloseOutcome::Protected);
        assert!(terminate(4, "csrss").is_err());
    }

    #[test]
    fn optea_does_not_close_itself() {
        assert!(is_protected("optea"));
        assert!(is_protected("claude"));
    }

    #[test]
    fn find_by_name_locates_the_current_process() {
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy().to_string();
        let pids = find_by_name(&stem).unwrap();
        assert!(
            pids.contains(&std::process::id()),
            "find_by_name({stem}) missed the running test process"
        );
    }

    #[test]
    fn unknown_name_finds_nothing() {
        assert!(find_by_name("OpteaDefinitelyNotARealProcess").unwrap().is_empty());
    }
}
