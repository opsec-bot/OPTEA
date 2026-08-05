//! Safe wrapper over PresentMonAPI2.
//!
//! The DLL is resolved and loaded at runtime rather than link-time, so OPTEA
//! runs — and `doctor` still works — on machines without PresentMon installed.
//! Absence is reported as [`CaptureError::NotInstalled`] with install guidance
//! rather than failing to start.
//!
//! See [`crate::ffi`] for why the metric table is treated as untrusted until
//! verified against a live service.

use crate::ffi::{self, Metric, PM_QUERY_ELEMENT, PM_STATUS, PM_VERSION};
use crate::stats::FrameSample;
use optea_sys::foreground::FocusMonitor;
use std::ffi::CString;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::core::PCSTR;
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// Frame times outside this range are physically implausible for a running
/// game: 2000 ms is a half-FPS slideshow, 0.05 ms is 20,000 FPS.
///
/// This is the load-bearing check on the untrusted metric table. A wrong
/// `PM_METRIC` ordinal reads some unrelated field — a temperature, a bitmask, a
/// pointer — and those essentially never land in this window.
pub const MIN_PLAUSIBLE_FRAME_MS: f64 = 0.05;
pub const MAX_PLAUSIBLE_FRAME_MS: f64 = 2000.0;

/// Fraction of implausible samples above which a capture is rejected outright.
const IMPLAUSIBLE_REJECT_RATIO: f64 = 0.10;

pub fn plausible_frame_time_ms(ms: f64) -> bool {
    ms.is_finite() && (MIN_PLAUSIBLE_FRAME_MS..=MAX_PLAUSIBLE_FRAME_MS).contains(&ms)
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error(
        "PresentMon is not installed (looked for {searched}). Install it from \
         https://github.com/GameTechDev/PresentMon/releases — the service and SDK are required \
         for any measurement."
    )]
    NotInstalled { searched: String },

    #[error("{symbol} missing from {dll} — the installed PresentMon is not API2 compatible")]
    MissingExport { dll: String, symbol: String },

    #[error("{call} failed: {status} ({code})")]
    Api {
        call: &'static str,
        status: &'static str,
        code: i32,
    },

    #[error(
        "PresentMon reports API version {found}, but OPTEA's metric table was transcribed \
         against {expected}. Metric ordinals may have shifted; refusing to record data that \
         could be silently wrong. Re-verify crates/optea-metrics/src/ffi.rs against the \
         installed PresentMonAPI.h."
    )]
    VersionMismatch { found: String, expected: String },

    #[error(
        "{bad} of {total} captured frame times were outside the plausible range \
         ({MIN_PLAUSIBLE_FRAME_MS}–{MAX_PLAUSIBLE_FRAME_MS} ms). This usually means the \
         PM_METRIC ordinal for {metric} is wrong for this PresentMon version — the query \
         succeeded but read the wrong field."
    )]
    ImplausibleData {
        bad: usize,
        total: usize,
        metric: &'static str,
    },

    #[error("no frames captured for process {pid} — is it running and presenting?")]
    NoFrames { pid: u32 },
}

type Result<T> = std::result::Result<T, CaptureError>;

fn check(call: &'static str, status: PM_STATUS) -> Result<()> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(CaptureError::Api {
            call,
            status: status.describe(),
            code: status.0,
        })
    }
}

/// Candidate locations for the API DLL, most specific first.
fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base in [
        r"C:\Program Files\Intel\PresentMon\SDK",
        r"C:\Program Files\Intel\PresentMon",
    ] {
        for dll in ["PresentMonAPI2Loader.dll", "PresentMonAPI2.dll"] {
            out.push(PathBuf::from(base).join(dll));
        }
    }
    // Bare names let the normal search order find a co-deployed copy.
    out.push(PathBuf::from("PresentMonAPI2Loader.dll"));
    out.push(PathBuf::from("PresentMonAPI2.dll"));
    out
}

/// True when a PresentMon API DLL can be found. Used by `doctor` to report
/// measurement availability without attempting a capture.
pub fn is_installed() -> bool {
    candidate_paths().iter().any(|p| p.is_absolute() && p.is_file())
}

/// Captured frames plus the focus record for the capture window.
///
/// The two travel together on purpose: a caller cannot obtain frames without
/// also receiving the evidence of whether they are trustworthy.
pub struct Capture {
    pub frames: Vec<FrameSample>,
    pub focus: FocusMonitor,
}

impl Capture {
    /// True when the game held focus throughout, so the numbers describe the
    /// game rather than a background throttle.
    pub fn is_trustworthy(&self) -> bool {
        self.focus.was_focused_throughout()
    }

    pub fn focus_note(&self) -> String {
        self.focus.describe()
    }
}

/// Result of end-to-end validation against the live service.
#[derive(Debug)]
pub struct Diagnostics {
    pub dll_path: String,
    pub api_version: (u16, u16, u16),
    pub expected_api: (u16, u16),
    pub session_opened: bool,
    /// Blob byte size the service reports for OPTEA's frame query.
    pub blob_size: Option<u32>,
    /// Byte offset the service assigns each metric within a frame blob.
    pub offsets: Vec<(&'static str, u64)>,
}

/// Load the DLL, open a session, and register OPTEA's real frame query.
///
/// This is the check that turns the transcribed FFI from "believed correct" into
/// "observed working": if any struct layout or signature were wrong, registering
/// the query would fail or report nonsense offsets.
pub fn diagnose() -> Result<Diagnostics> {
    let lib = Library::load()?;
    let dll_path = lib.path().to_string();
    let api_version = lib.api_version()?;

    let mut handle: ffi::PM_SESSION_HANDLE = std::ptr::null_mut();
    check("pmOpenSession", unsafe { (lib.open_session)(&mut handle) })?;

    let query = FrameQuery::register(&lib, handle);
    let (blob_size, offsets) = match &query {
        Ok(q) => (
            Some(q.blob_size),
            vec![
                (Metric::CpuFrameTime.symbol(), q.off_frame_time),
                (Metric::GpuBusy.symbol(), q.off_gpu_busy),
                (Metric::AllInputToPhotonLatency.symbol(), q.off_latency),
            ],
        ),
        Err(_) => (None, Vec::new()),
    };
    if let Ok(mut q) = query {
        q.free(&lib);
    }

    unsafe {
        let _ = (lib.close_session)(handle);
    }

    Ok(Diagnostics {
        dll_path,
        api_version,
        expected_api: ffi::GENERATED_AGAINST,
        session_opened: true,
        blob_size,
        offsets,
    })
}

/// Dynamically loaded PresentMonAPI2 entry points.
pub struct Library {
    module: HMODULE,
    path: String,
    open_session: ffi::PfnOpenSession,
    close_session: ffi::PfnCloseSession,
    start_tracking: ffi::PfnStartTrackingProcess,
    stop_tracking: ffi::PfnStopTrackingProcess,
    register_frame_query: ffi::PfnRegisterFrameQuery,
    consume_frames: ffi::PfnConsumeFrames,
    free_frame_query: ffi::PfnFreeFrameQuery,
    get_api_version: ffi::PfnGetApiVersion,
}

impl Library {
    pub fn load() -> Result<Self> {
        let candidates = candidate_paths();
        for path in &candidates {
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let module = match unsafe { LoadLibraryW(windows::core::PCWSTR(wide.as_ptr())) } {
                Ok(m) if !m.is_invalid() => m,
                _ => continue,
            };
            return Self::bind(module, path.display().to_string());
        }
        Err(CaptureError::NotInstalled {
            searched: candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("; "),
        })
    }

    fn bind(module: HMODULE, path: String) -> Result<Self> {
        // SAFETY: each symbol is transmuted to the signature transcribed from
        // PresentMonAPI.h. A mismatch here is undefined behaviour, which is why
        // ffi.rs documents its provenance.
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let cname = CString::new($name).expect("symbol name has no interior NUL");
                let addr = unsafe { GetProcAddress(module, PCSTR(cname.as_ptr() as *const u8)) };
                match addr {
                    Some(p) => unsafe { std::mem::transmute::<_, $ty>(p) },
                    None => {
                        unsafe {
                            let _ = FreeLibrary(module);
                        }
                        return Err(CaptureError::MissingExport {
                            dll: path.clone(),
                            symbol: $name.to_string(),
                        });
                    }
                }
            }};
        }

        Ok(Library {
            open_session: sym!("pmOpenSession", ffi::PfnOpenSession),
            close_session: sym!("pmCloseSession", ffi::PfnCloseSession),
            start_tracking: sym!("pmStartTrackingProcess", ffi::PfnStartTrackingProcess),
            stop_tracking: sym!("pmStopTrackingProcess", ffi::PfnStopTrackingProcess),
            register_frame_query: sym!("pmRegisterFrameQuery", ffi::PfnRegisterFrameQuery),
            consume_frames: sym!("pmConsumeFrames", ffi::PfnConsumeFrames),
            free_frame_query: sym!("pmFreeFrameQuery", ffi::PfnFreeFrameQuery),
            get_api_version: sym!("pmGetApiVersion", ffi::PfnGetApiVersion),
            module,
            path,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn api_version(&self) -> Result<(u16, u16, u16)> {
        let mut v = PM_VERSION::default();
        check("pmGetApiVersion", unsafe { (self.get_api_version)(&mut v) })?;
        Ok((v.major, v.minor, v.patch))
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.module);
        }
    }
}

/// An open PresentMon service session.
pub struct Session {
    lib: Library,
    handle: ffi::PM_SESSION_HANDLE,
    tracked: Vec<u32>,
}

impl Session {
    /// Open a session and verify the service matches our transcribed metric table.
    pub fn open() -> Result<Self> {
        let lib = Library::load()?;

        let (major, minor, _patch) = lib.api_version()?;
        let (exp_major, exp_minor) = ffi::GENERATED_AGAINST;
        if (major, minor) != (exp_major, exp_minor) {
            return Err(CaptureError::VersionMismatch {
                found: format!("{major}.{minor}"),
                expected: format!("{exp_major}.{exp_minor}"),
            });
        }

        let mut handle: ffi::PM_SESSION_HANDLE = std::ptr::null_mut();
        check("pmOpenSession", unsafe { (lib.open_session)(&mut handle) })?;

        Ok(Session {
            lib,
            handle,
            tracked: Vec::new(),
        })
    }

    pub fn track(&mut self, pid: u32) -> Result<()> {
        check("pmStartTrackingProcess", unsafe {
            (self.lib.start_tracking)(self.handle, pid)
        })?;
        self.tracked.push(pid);
        Ok(())
    }

    /// Capture frames for `duration`, polling at `poll` intervals.
    ///
    /// Focus is sampled throughout, because a backgrounded game is usually
    /// throttled and would otherwise yield plausible-looking nonsense.
    pub fn capture(
        &mut self,
        pid: u32,
        duration: Duration,
        poll: Duration,
    ) -> Result<Capture> {
        let mut query = FrameQuery::register(&self.lib, self.handle)?;
        let result = self.capture_loop(&mut query, pid, duration, poll);
        // Free the query even when the capture failed part-way through.
        query.free(&self.lib);
        let (frames, focus) = result?;

        if frames.is_empty() {
            return Err(CaptureError::NoFrames { pid });
        }
        validate(&frames)?;
        Ok(Capture { frames, focus })
    }

    fn capture_loop(
        &self,
        query: &mut FrameQuery,
        pid: u32,
        duration: Duration,
        poll: Duration,
    ) -> Result<(Vec<FrameSample>, FocusMonitor)> {
        let mut frames = Vec::new();
        let mut focus = FocusMonitor::new(pid);
        let deadline = Instant::now() + duration;

        while Instant::now() < deadline {
            focus.sample();
            frames.extend(query.consume(&self.lib, pid)?);
            std::thread::sleep(poll);
        }
        focus.sample();
        // Final drain, so frames presented in the last poll window are not lost.
        frames.extend(query.consume(&self.lib, pid)?);
        Ok((frames, focus))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for pid in std::mem::take(&mut self.tracked) {
            unsafe {
                let _ = (self.lib.stop_tracking)(self.handle, pid);
            }
        }
        if !self.handle.is_null() {
            unsafe {
                let _ = (self.lib.close_session)(self.handle);
            }
        }
    }
}

/// Reject a capture whose frame times are mostly nonsense — the signature of a
/// stale `PM_METRIC` ordinal reading an unrelated field.
fn validate(frames: &[FrameSample]) -> Result<()> {
    let bad = frames
        .iter()
        .filter(|f| !plausible_frame_time_ms(f.frame_time_ms))
        .count();
    if bad as f64 / frames.len() as f64 > IMPLAUSIBLE_REJECT_RATIO {
        return Err(CaptureError::ImplausibleData {
            bad,
            total: frames.len(),
            metric: Metric::CpuFrameTime.symbol(),
        });
    }
    Ok(())
}

/// A registered per-frame query and its blob layout.
struct FrameQuery {
    handle: ffi::PM_FRAME_QUERY_HANDLE,
    blob_size: u32,
    /// Byte offsets within each blob, as filled in by `pmRegisterFrameQuery`.
    off_frame_time: u64,
    off_gpu_busy: u64,
    off_latency: u64,
}

/// Frames pulled per `pmConsumeFrames` call.
const BATCH_FRAMES: u32 = 512;

impl FrameQuery {
    fn register(lib: &Library, session: ffi::PM_SESSION_HANDLE) -> Result<Self> {
        let mut elements = [
            PM_QUERY_ELEMENT::per_frame(Metric::CpuFrameTime),
            PM_QUERY_ELEMENT::per_frame(Metric::GpuBusy),
            PM_QUERY_ELEMENT::per_frame(Metric::AllInputToPhotonLatency),
        ];
        let mut handle: ffi::PM_FRAME_QUERY_HANDLE = std::ptr::null_mut();
        let mut blob_size: u32 = 0;

        // `data_offset`/`data_size` on each element are outputs: the service
        // fills in where each metric lands inside a frame blob.
        check("pmRegisterFrameQuery", unsafe {
            (lib.register_frame_query)(
                session,
                &mut handle,
                elements.as_mut_ptr(),
                elements.len() as u64,
                &mut blob_size,
            )
        })?;

        if blob_size == 0 {
            return Err(CaptureError::Api {
                call: "pmRegisterFrameQuery",
                status: "service reported a zero-byte frame blob",
                code: 0,
            });
        }

        Ok(FrameQuery {
            handle,
            blob_size,
            off_frame_time: elements[0].data_offset,
            off_gpu_busy: elements[1].data_offset,
            off_latency: elements[2].data_offset,
        })
    }

    fn free(&mut self, lib: &Library) {
        if !self.handle.is_null() {
            unsafe {
                let _ = (lib.free_frame_query)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }

    fn consume(&mut self, lib: &Library, pid: u32) -> Result<Vec<FrameSample>> {
        let mut count = BATCH_FRAMES;
        let mut buf = vec![0u8; (self.blob_size as usize) * BATCH_FRAMES as usize];

        check("pmConsumeFrames", unsafe {
            (lib.consume_frames)(self.handle, pid, buf.as_mut_ptr(), &mut count)
        })?;

        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let blob = &buf[i * self.blob_size as usize..(i + 1) * self.blob_size as usize];
            out.push(FrameSample {
                frame_time_ms: read_f64(blob, self.off_frame_time).unwrap_or(f64::NAN),
                gpu_busy_ms: read_f64(blob, self.off_gpu_busy).filter(|v| v.is_finite()),
                input_latency_ms: read_f64(blob, self.off_latency)
                    .filter(|v| v.is_finite() && *v > 0.0),
            });
        }
        Ok(out)
    }
}

/// Read a little-endian `f64` at `offset`, bounds-checked.
///
/// Offsets come from the service, so they are treated as untrusted input rather
/// than assumed to fit.
fn read_f64(blob: &[u8], offset: u64) -> Option<f64> {
    let start = offset as usize;
    let end = start.checked_add(8)?;
    if end > blob.len() {
        return None;
    }
    Some(f64::from_le_bytes(blob[start..end].try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plausibility_window_rejects_garbage() {
        assert!(plausible_frame_time_ms(6.94)); // ~144 fps
        assert!(plausible_frame_time_ms(16.67)); // 60 fps
        assert!(plausible_frame_time_ms(100.0)); // bad, but a real frame

        // The shapes a wrong metric ordinal actually produces:
        assert!(!plausible_frame_time_ms(0.0));
        assert!(!plausible_frame_time_ms(-1.0));
        assert!(!plausible_frame_time_ms(f64::NAN));
        assert!(!plausible_frame_time_ms(f64::INFINITY));
        assert!(!plausible_frame_time_ms(1.5e18)); // a QPC timestamp
        assert!(!plausible_frame_time_ms(65.0e6)); // a frequency in Hz
    }

    #[test]
    fn validate_accepts_a_clean_capture() {
        let frames: Vec<FrameSample> = (0..100).map(|_| FrameSample::new(6.94)).collect();
        assert!(validate(&frames).is_ok());
    }

    #[test]
    fn validate_tolerates_a_few_odd_frames() {
        // Real captures contain the occasional zero; a handful must not abort a run.
        let mut frames: Vec<FrameSample> = (0..100).map(|_| FrameSample::new(6.94)).collect();
        frames[0] = FrameSample::new(0.0);
        frames[1] = FrameSample::new(0.0);
        assert!(validate(&frames).is_ok());
    }

    #[test]
    fn validate_rejects_a_wrong_metric_ordinal() {
        // Reading a QPC timestamp field instead of a frame time.
        let frames: Vec<FrameSample> = (0..100).map(|_| FrameSample::new(1.5e18)).collect();
        let err = validate(&frames).unwrap_err();
        assert!(
            matches!(err, CaptureError::ImplausibleData { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("PM_METRIC_CPU_FRAME_TIME"));
    }

    #[test]
    fn read_f64_is_bounds_checked() {
        let blob = [0u8; 16];
        assert!(read_f64(&blob, 0).is_some());
        assert!(read_f64(&blob, 8).is_some());
        assert!(read_f64(&blob, 9).is_none(), "must reject a partial read");
        assert!(read_f64(&blob, 16).is_none());
        assert!(read_f64(&blob, u64::MAX).is_none(), "must not overflow");
    }

    #[test]
    fn read_f64_decodes_little_endian() {
        let mut blob = vec![0u8; 16];
        blob[8..16].copy_from_slice(&6.94f64.to_le_bytes());
        assert_eq!(read_f64(&blob, 8), Some(6.94));
    }

    #[test]
    fn reports_not_installed_with_guidance() {
        // PresentMon is absent on this machine; the error must say where to get it.
        if !is_installed() {
            let msg = match Library::load() {
                Ok(_) => panic!("load succeeded though no DLL was found on disk"),
                Err(e) => e.to_string(),
            };
            assert!(msg.contains("not installed"), "{msg}");
            assert!(msg.contains("github.com/GameTechDev/PresentMon"), "{msg}");
        }
    }
}
