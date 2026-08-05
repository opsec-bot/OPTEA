//! Power scheme and processor power-policy queries.

use crate::error::{Result, SysError};
use serde::{Deserialize, Serialize};
use windows::core::GUID;
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadFriendlyName,
};

// Well-known scheme GUIDs.
pub const SCHEME_BALANCED: GUID = GUID::from_u128(0x381b4222_f694_41f0_9685_ff5bb260df2e);
pub const SCHEME_HIGH_PERFORMANCE: GUID = GUID::from_u128(0x8c5e7fda_e8bf_4a96_9a85_a6e23a8c635c);
pub const SCHEME_POWER_SAVER: GUID = GUID::from_u128(0xa1841308_3541_4fab_bc81_f71556f20b4a);
/// Hidden by default; must be unlocked with `powercfg -duplicatescheme`.
pub const SCHEME_ULTIMATE_PERFORMANCE: GUID =
    GUID::from_u128(0xe9a42b02_d5df_448d_aa00_03f14749eb61);

pub const SUBGROUP_PROCESSOR: GUID = GUID::from_u128(0x54533251_82be_4824_96c1_47b60b740d00);
/// Core-parking floor, as a percentage of cores kept unparked. 100 == parking off.
pub const SETTING_CP_MIN_CORES: GUID = GUID::from_u128(0x0cc5b647_c1df_4637_891a_dec35c318583);
/// Processor performance boost mode.
pub const SETTING_PERF_BOOST_MODE: GUID = GUID::from_u128(0xbe337238_0d82_4146_a960_4f3749d470c7);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerState {
    pub scheme_guid: String,
    pub scheme_name: String,
    /// `CPMINCORES` on AC. 100 means core parking is fully disabled.
    pub min_cores_percent_ac: Option<u32>,
    pub perf_boost_mode_ac: Option<u32>,
}

impl PowerState {
    pub fn query() -> Result<Self> {
        let scheme = active_scheme()?;
        Ok(PowerState {
            scheme_guid: guid_to_string(&scheme),
            scheme_name: friendly_name(&scheme).unwrap_or_else(|_| "Unknown".into()),
            min_cores_percent_ac: read_ac_value(&scheme, &SUBGROUP_PROCESSOR, &SETTING_CP_MIN_CORES)
                .ok(),
            perf_boost_mode_ac: read_ac_value(
                &scheme,
                &SUBGROUP_PROCESSOR,
                &SETTING_PERF_BOOST_MODE,
            )
            .ok(),
        })
    }

    pub fn is_high_performance(&self) -> bool {
        let g = self.scheme_guid.to_lowercase();
        g.contains("8c5e7fda") || g.contains("e9a42b02")
    }

    pub fn is_ultimate_performance(&self) -> bool {
        self.scheme_guid.to_lowercase().contains("e9a42b02")
    }

    /// Core parking is off when the floor is 100% of cores.
    pub fn core_parking_disabled(&self) -> bool {
        self.min_cores_percent_ac == Some(100)
    }
}

/// Canonical lowercase `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` rendering.
///
/// Written out rather than leaning on `Debug`, since these strings are compared
/// against well-known scheme GUIDs and persisted into snapshots.
pub fn guid_to_string(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

pub fn active_scheme() -> Result<GUID> {
    let mut ptr: *mut GUID = std::ptr::null_mut();
    let status = unsafe { PowerGetActiveScheme(None, &mut ptr) };
    if status != ERROR_SUCCESS || ptr.is_null() {
        return Err(SysError::msg(format!(
            "PowerGetActiveScheme failed with status {status:?}"
        )));
    }
    let guid = unsafe { *ptr };
    unsafe {
        let _ = LocalFree(HLOCAL(ptr as *mut std::ffi::c_void));
    }
    Ok(guid)
}

pub fn friendly_name(scheme: &GUID) -> Result<String> {
    let mut cb: u32 = 0;
    // Size probe. The buffer is UTF-16 bytes, not characters.
    let _ = unsafe { PowerReadFriendlyName(None, Some(scheme), None, None, None, &mut cb) };
    if cb == 0 {
        return Err(SysError::msg("PowerReadFriendlyName reported zero size"));
    }

    let mut buf = vec![0u8; cb as usize];
    let status = unsafe {
        PowerReadFriendlyName(
            None,
            Some(scheme),
            None,
            None,
            Some(buf.as_mut_ptr()),
            &mut cb,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(SysError::msg(format!(
            "PowerReadFriendlyName failed with status {status:?}"
        )));
    }

    let wide: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(crate::wide::from_wide_nul(&wide))
}

pub fn read_ac_value(scheme: &GUID, subgroup: &GUID, setting: &GUID) -> Result<u32> {
    let mut value: u32 = 0;
    let status = unsafe {
        PowerReadACValueIndex(None, Some(scheme), Some(subgroup), Some(setting), &mut value)
    };
    if status != ERROR_SUCCESS {
        return Err(SysError::msg(format!(
            "PowerReadACValueIndex failed with status {status:?}"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_active_scheme() {
        let scheme = active_scheme().unwrap();
        assert_ne!(scheme, GUID::zeroed());
    }

    #[test]
    fn reads_power_state() {
        let state = PowerState::query().unwrap();
        assert!(!state.scheme_name.is_empty(), "scheme name should resolve");
        // Core-parking floor is a percentage.
        if let Some(pct) = state.min_cores_percent_ac {
            assert!(pct <= 100, "implausible CPMINCORES {pct}");
        }
    }

    #[test]
    fn ultimate_implies_high_performance() {
        let state = PowerState {
            scheme_guid: "e9a42b02-d5df-448d-aa00-03f14749eb61".into(),
            scheme_name: "Ultimate Performance".into(),
            min_cores_percent_ac: Some(100),
            perf_boost_mode_ac: None,
        };
        assert!(state.is_ultimate_performance());
        assert!(state.is_high_performance());
        assert!(state.core_parking_disabled());
    }
}
