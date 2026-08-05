//! Raw FFI declarations for PresentMonAPI2.
//!
//! # Provenance and risk
//!
//! Verified line-by-line against the installed SDK header at
//! `C:\Program Files\Intel\PresentMon\SDK\PresentMonAPI.h` from PresentMon
//! **2.5.1**, which declares `PM_API_VERSION 3.3`. All struct layouts and
//! function signatures match.
//!
//! `PM_METRIC` is a plain C enum, so every value is positional: inserting a
//! metric anywhere in the upstream list shifts every later ordinal. A stale
//! table would not fail loudly — it would silently read the *wrong metric* and
//! feed plausible-looking garbage into the statistics layer.
//!
//! That risk is real rather than theoretical: the `main`-branch header carries
//! six metrics this release does not (`PSO_COMPILE_*`, `CPU_CORE_TEMPERATURE`,
//! …), moving `PM_METRIC_COUNT_` from 91 to 95. The ordinals below survived only
//! because Intel *appended* them past the ones OPTEA reads. An insert anywhere
//! earlier would have silently repointed every metric here.
//!
//! Three defences, because a comparison harness that reports confident numbers
//! from the wrong field is worse than no harness at all:
//!
//! 1. [`GENERATED_AGAINST`] records the upstream version this table came from,
//!    and [`super::presentmon::Session::open`] compares it against the live
//!    `pmGetApiVersion`.
//! 2. Metrics are looked up through [`Metric::ordinal`] by name, so the mapping
//!    lives in exactly one auditable place.
//! 3. Captured values are range-checked before use; see
//!    [`super::presentmon::plausible_frame_time_ms`].

#![allow(non_camel_case_types)]

use std::ffi::c_void;

/// `PM_API_VERSION_MAJOR` / `_MINOR` this metric table was verified against.
///
/// Note this is the **API** version from the header, which is not the product
/// version: PresentMon 2.5.1 ships API 3.3.
pub const GENERATED_AGAINST: (u16, u16) = (3, 3);

/// `PM_METRIC_COUNT_` in the verified header. Ordinals at or above this are
/// invalid and indicate the table has drifted from the installed service.
pub const METRIC_COUNT: i32 = 91;

pub type PM_SESSION_HANDLE = *mut c_void;
pub type PM_FRAME_QUERY_HANDLE = *mut c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PM_STATUS(pub i32);

impl PM_STATUS {
    pub const SUCCESS: PM_STATUS = PM_STATUS(0);

    pub fn is_ok(self) -> bool {
        self == PM_STATUS::SUCCESS
    }

    /// Names taken from the `PM_STATUS` enum, which starts at 0 and is dense.
    pub fn describe(self) -> &'static str {
        match self.0 {
            0 => "success",
            1 => "failure",
            2 => "bad argument",
            3 => "bad handle",
            4 => "service error",
            5 => "invalid ETL file",
            6 => "invalid PID",
            7 => "already tracking process",
            8 => "unable to create NSM",
            9 => "invalid adapter id",
            10 => "out of range",
            11 => "insufficient buffer",
            12 => "pipe error",
            13 => "session not open",
            14 => "middleware missing path",
            15 => "nonexistent file path",
            16 => "middleware invalid signature",
            17 => "middleware missing endpoint",
            18 => "middleware version low",
            19 => "middleware version high",
            20 => "middleware/service mismatch",
            21 => "query malformed",
            22 => "mode mismatch",
            23 => "feature disabled",
            _ => "unknown status",
        }
    }
}

/// `PM_STAT`. Frame queries use `NONE` to read raw per-frame values.
pub const PM_STAT_NONE: i32 = 0;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PM_QUERY_ELEMENT {
    pub metric: i32,
    pub stat: i32,
    pub device_id: u32,
    pub array_index: u32,
    /// Filled in by `pmRegisterFrameQuery`; do not set.
    pub data_offset: u64,
    /// Filled in by `pmRegisterFrameQuery`; do not set.
    pub data_size: u64,
}

impl PM_QUERY_ELEMENT {
    pub fn per_frame(metric: Metric) -> Self {
        PM_QUERY_ELEMENT {
            metric: metric.ordinal(),
            stat: PM_STAT_NONE,
            device_id: 0,
            array_index: 0,
            data_offset: 0,
            data_size: 0,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PM_VERSION {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub tag: [u8; 22],
    pub hash: [u8; 8],
    pub config: [u8; 4],
}

impl Default for PM_VERSION {
    fn default() -> Self {
        PM_VERSION {
            major: 0,
            minor: 0,
            patch: 0,
            tag: [0; 22],
            hash: [0; 8],
            config: [0; 4],
        }
    }
}

/// The subset of `PM_METRIC` OPTEA queries.
///
/// Ordinals are positional in the upstream C enum. Keep this list and
/// [`Metric::ordinal`] adjacent so a version bump is a single-site edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// Frame-pacing interval in milliseconds — the primary FPS input.
    CpuFrameTime,
    /// Milliseconds the GPU spent actively working on the frame.
    GpuBusy,
    /// Mouse-click-to-photon latency, when the title reports it.
    ClickToPhotonLatency,
    /// Any-input-to-photon latency; broader and more often populated.
    AllInputToPhotonLatency,
}

impl Metric {
    pub const fn ordinal(self) -> i32 {
        match self {
            Metric::CpuFrameTime => 8,
            Metric::GpuBusy => 14,
            Metric::ClickToPhotonLatency => 25,
            Metric::AllInputToPhotonLatency => 65,
        }
    }

    pub const fn symbol(self) -> &'static str {
        match self {
            Metric::CpuFrameTime => "PM_METRIC_CPU_FRAME_TIME",
            Metric::GpuBusy => "PM_METRIC_GPU_BUSY",
            Metric::ClickToPhotonLatency => "PM_METRIC_CLICK_TO_PHOTON_LATENCY",
            Metric::AllInputToPhotonLatency => "PM_METRIC_ALL_INPUT_TO_PHOTON_LATENCY",
        }
    }
}

// Function pointer types, matching PresentMonAPI.h exactly.
pub type PfnOpenSession = unsafe extern "C" fn(*mut PM_SESSION_HANDLE) -> PM_STATUS;
pub type PfnCloseSession = unsafe extern "C" fn(PM_SESSION_HANDLE) -> PM_STATUS;
pub type PfnStartTrackingProcess = unsafe extern "C" fn(PM_SESSION_HANDLE, u32) -> PM_STATUS;
pub type PfnStopTrackingProcess = unsafe extern "C" fn(PM_SESSION_HANDLE, u32) -> PM_STATUS;
pub type PfnRegisterFrameQuery = unsafe extern "C" fn(
    PM_SESSION_HANDLE,
    *mut PM_FRAME_QUERY_HANDLE,
    *mut PM_QUERY_ELEMENT,
    u64,
    *mut u32,
) -> PM_STATUS;
pub type PfnConsumeFrames =
    unsafe extern "C" fn(PM_FRAME_QUERY_HANDLE, u32, *mut u8, *mut u32) -> PM_STATUS;
pub type PfnFreeFrameQuery = unsafe extern "C" fn(PM_FRAME_QUERY_HANDLE) -> PM_STATUS;
pub type PfnGetApiVersion = unsafe extern "C" fn(*mut PM_VERSION) -> PM_STATUS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_element_matches_c_layout() {
        // 2 x i32 + 2 x u32 + 2 x u64 with 8-byte alignment.
        assert_eq!(std::mem::size_of::<PM_QUERY_ELEMENT>(), 32);
        assert_eq!(std::mem::align_of::<PM_QUERY_ELEMENT>(), 8);
    }

    #[test]
    fn version_struct_matches_c_layout() {
        // 3 x uint16 + char[22] + char[8] + char[4], packed to 1-byte alignment.
        assert_eq!(std::mem::size_of::<PM_VERSION>(), 40);
        assert_eq!(std::mem::align_of::<PM_VERSION>(), 2);
    }

    #[test]
    fn metric_ordinals_are_distinct() {
        let all = [
            Metric::CpuFrameTime,
            Metric::GpuBusy,
            Metric::ClickToPhotonLatency,
            Metric::AllInputToPhotonLatency,
        ];
        let mut seen = Vec::new();
        for m in all {
            assert!(
                !seen.contains(&m.ordinal()),
                "duplicate ordinal {} for {}",
                m.ordinal(),
                m.symbol()
            );
            seen.push(m.ordinal());
        }
    }

    #[test]
    fn metric_ordinals_are_in_range() {
        for m in [
            Metric::CpuFrameTime,
            Metric::GpuBusy,
            Metric::ClickToPhotonLatency,
            Metric::AllInputToPhotonLatency,
        ] {
            assert!(
                (0..METRIC_COUNT).contains(&m.ordinal()),
                "{} ordinal {} is outside PM_METRIC_COUNT_ ({METRIC_COUNT})",
                m.symbol(),
                m.ordinal()
            );
        }
    }

    /// Ordinals as counted in the installed 2.5.1 header. Spelled out so a
    /// future version bump has to consciously re-verify each one rather than
    /// quietly inherit a stale value.
    #[test]
    fn metric_ordinals_match_the_verified_header() {
        assert_eq!(Metric::CpuFrameTime.ordinal(), 8);
        assert_eq!(Metric::GpuBusy.ordinal(), 14);
        assert_eq!(Metric::ClickToPhotonLatency.ordinal(), 25);
        assert_eq!(Metric::AllInputToPhotonLatency.ordinal(), 65);
    }

    #[test]
    fn per_frame_element_requests_raw_values() {
        let e = PM_QUERY_ELEMENT::per_frame(Metric::CpuFrameTime);
        assert_eq!(e.stat, PM_STAT_NONE, "frame queries must use PM_STAT_NONE");
        assert_eq!(e.data_offset, 0, "offset is an output field");
        assert_eq!(e.data_size, 0, "size is an output field");
    }

    #[test]
    fn status_zero_is_success() {
        assert!(PM_STATUS(0).is_ok());
        assert!(!PM_STATUS(1).is_ok());
        assert_eq!(PM_STATUS(13).describe(), "session not open");
    }
}
