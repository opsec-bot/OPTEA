//! Display-adapter enumeration including devices that are present but faulted.
//!
//! [`display`](crate::display) only sees adapters that are actually driving an
//! output. This module walks the device tree instead, so an adapter sitting in a
//! problem state (a disabled iGPU, a driver that failed to start) is still
//! visible and can be reported rather than silently omitted.

use crate::error::{Result, SysError};
use crate::wide::from_wide_nul;
use serde::{Deserialize, Serialize};
use windows::core::GUID;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Status, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, CM_DEVNODE_STATUS_FLAGS, CM_PROB,
    CR_SUCCESS, DIGCF_PRESENT, HDEVINFO, SETUP_DI_REGISTRY_PROPERTY, SPDRP_DEVICEDESC,
    SPDRP_FRIENDLYNAME, SP_DEVINFO_DATA,
};

/// `GUID_DEVCLASS_DISPLAY`
const DEVCLASS_DISPLAY: GUID = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);

/// Device node has a problem recorded.
const DN_HAS_PROBLEM: u32 = 0x0000_0400;

/// Windows device problem codes worth naming.
pub fn problem_description(code: u32) -> &'static str {
    match code {
        0 => "no problem",
        10 => "device cannot start (code 10)",
        12 => "insufficient free resources (code 12)",
        18 => "drivers need reinstalling (code 18)",
        22 => "device is disabled (code 22)",
        28 => "drivers not installed (code 28)",
        31 => "device not working properly (code 31)",
        43 => "driver reported a failure (code 43)",
        _ => "unknown problem code",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuDevice {
    pub name: String,
    /// `None` when the device is healthy.
    pub problem_code: Option<u32>,
}

impl GpuDevice {
    pub fn is_healthy(&self) -> bool {
        self.problem_code.is_none()
    }

    pub fn problem_text(&self) -> Option<&'static str> {
        self.problem_code.map(problem_description)
    }

    pub fn is_nvidia(&self) -> bool {
        self.name.to_lowercase().contains("nvidia")
    }

    /// Heuristic: AMD APU and Intel on-die graphics report these family names.
    pub fn looks_integrated(&self) -> bool {
        let n = self.name.to_lowercase();
        n.contains("vega") && n.contains("graphics")
            || n.contains("radeon(tm) graphics")
            || n.contains("intel(r) hd graphics")
            || n.contains("intel(r) uhd graphics")
            || n.contains("intel(r) iris")
    }
}

/// Enumerate present display adapters, healthy or not.
pub fn enumerate() -> Result<Vec<GpuDevice>> {
    let devinfo = unsafe {
        SetupDiGetClassDevsW(Some(&DEVCLASS_DISPLAY), None, None, DIGCF_PRESENT)
            .map_err(|e| SysError::api("SetupDiGetClassDevsW", e))?
    };

    let mut out = Vec::new();
    for index in 0.. {
        let mut data = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInfo(devinfo, index, &mut data) }.is_err() {
            break; // ERROR_NO_MORE_ITEMS
        }

        let name = device_string(devinfo, &data, SPDRP_FRIENDLYNAME)
            .or_else(|| device_string(devinfo, &data, SPDRP_DEVICEDESC))
            .unwrap_or_else(|| "Unknown display adapter".into());

        out.push(GpuDevice {
            name,
            problem_code: node_problem(data.DevInst),
        });
    }

    unsafe {
        let _ = SetupDiDestroyDeviceInfoList(devinfo);
    }
    Ok(out)
}

fn device_string(
    devinfo: HDEVINFO,
    data: &SP_DEVINFO_DATA,
    property: SETUP_DI_REGISTRY_PROPERTY,
) -> Option<String> {
    let mut buf = [0u8; 512];
    let mut required = 0u32;
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            devinfo,
            data,
            property,
            None,
            Some(&mut buf),
            Some(&mut required),
        )
    };
    if ok.is_err() {
        return None;
    }
    let wide: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let s = from_wide_nul(&wide);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn node_problem(devinst: u32) -> Option<u32> {
    let mut status = CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = CM_PROB(0);
    let cr = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) };
    if cr != CR_SUCCESS {
        return None;
    }
    if status.0 & DN_HAS_PROBLEM != 0 && problem.0 != 0 {
        Some(problem.0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_display_adapters() {
        let gpus = enumerate().unwrap();
        assert!(!gpus.is_empty(), "expected at least one display adapter");
        for g in &gpus {
            assert!(!g.name.is_empty());
        }
    }

    #[test]
    fn healthy_devices_have_no_problem_text() {
        for g in enumerate().unwrap() {
            assert_eq!(g.is_healthy(), g.problem_text().is_none());
        }
    }

    #[test]
    fn names_known_problem_codes() {
        assert_eq!(problem_description(43), "driver reported a failure (code 43)");
        assert_eq!(problem_description(22), "device is disabled (code 22)");
    }
}
