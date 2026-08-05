//! Active display enumeration: which GPU drives each output, over what
//! connector, at what mode.
//!
//! This matters more than it looks. A monitor accidentally plugged into a
//! motherboard output routes every frame through the iGPU, which costs far more
//! FPS and latency than any registry tweak can recover — so OPTEA verifies it
//! before recommending anything else.

use crate::error::{Result, SysError};
use crate::wide::{from_wide_nul, wide};
use serde::{Deserialize, Serialize};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplaySettingsW, DEVMODEW, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS,
};

/// Physical connector carrying a display signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Connector {
    Vga,
    Dvi,
    Hdmi,
    DisplayPort,
    EmbeddedDisplayPort,
    Internal,
    Other(i32),
}

impl Connector {
    fn from_raw(v: i32) -> Self {
        match v {
            0 => Connector::Vga,
            4 => Connector::Dvi,
            5 => Connector::Hdmi,
            10 => Connector::DisplayPort,
            11 => Connector::EmbeddedDisplayPort,
            v if v == i32::from_le_bytes(0x8000_0000u32.to_le_bytes()) => Connector::Internal,
            other => Connector::Other(other),
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            Connector::Vga => "VGA".into(),
            Connector::Dvi => "DVI".into(),
            Connector::Hdmi => "HDMI".into(),
            Connector::DisplayPort => "DisplayPort".into(),
            Connector::EmbeddedDisplayPort => "eDP".into(),
            Connector::Internal => "Internal".into(),
            Connector::Other(v) => format!("Other({v})"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub gdi_name: String,
    /// EDID friendly name, when the monitor reports one.
    pub monitor_name: Option<String>,
    /// Adapter driving this output, e.g. "NVIDIA GeForce GTX 1660 SUPER".
    pub gpu_name: String,
    pub connector: Option<Connector>,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub is_primary: bool,
}

impl DisplayInfo {
    pub fn mode_string(&self) -> String {
        format!(
            "{}x{} @ {:.0} Hz",
            self.width, self.height, self.refresh_hz
        )
    }
}

/// Enumerate every display currently attached to the desktop.
pub fn enumerate() -> Result<Vec<DisplayInfo>> {
    let adapters = enumerate_gdi_adapters();
    let config = query_display_config().unwrap_or_default();

    let mut out = Vec::new();
    for (gdi_name, gpu_name, is_primary) in adapters {
        let Some((width, height, dev_refresh)) = current_mode(&gdi_name) else {
            continue;
        };
        let cfg = config.iter().find(|c| c.gdi_name == gdi_name);
        out.push(DisplayInfo {
            monitor_name: cfg.and_then(|c| c.monitor_name.clone()),
            connector: cfg.map(|c| c.connector),
            // The path's rational refresh is exact; DEVMODE rounds to integer Hz.
            refresh_hz: cfg
                .and_then(|c| c.refresh_hz)
                .unwrap_or(dev_refresh as f64),
            gdi_name,
            gpu_name,
            width,
            height,
            is_primary,
        });
    }
    Ok(out)
}

/// `(gdi_name, gpu_name, is_primary)` for adapters attached to the desktop.
fn enumerate_gdi_adapters() -> Vec<(String, String, bool)> {
    const ATTACHED_TO_DESKTOP: u32 = 0x0000_0001;
    const PRIMARY_DEVICE: u32 = 0x0000_0004;

    let mut out = Vec::new();
    for i in 0.. {
        let mut dd = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        let ok = unsafe { EnumDisplayDevicesW(None, i, &mut dd, 0) };
        if !ok.as_bool() {
            break;
        }
        if dd.StateFlags & ATTACHED_TO_DESKTOP == 0 {
            continue;
        }
        out.push((
            from_wide_nul(&dd.DeviceName),
            from_wide_nul(&dd.DeviceString),
            dd.StateFlags & PRIMARY_DEVICE != 0,
        ));
    }
    out
}

fn current_mode(gdi_name: &str) -> Option<(u32, u32, u32)> {
    let name = wide(gdi_name);
    let mut dm = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    let ok = unsafe { EnumDisplaySettingsW(name.as_pcwstr(), ENUM_CURRENT_SETTINGS, &mut dm) };
    if !ok.as_bool() {
        return None;
    }
    Some((dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency))
}

#[derive(Debug, Clone)]
struct PathConfig {
    gdi_name: String,
    monitor_name: Option<String>,
    connector: Connector,
    refresh_hz: Option<f64>,
}

fn query_display_config() -> Result<Vec<PathConfig>> {
    let mut n_paths = 0u32;
    let mut n_modes = 0u32;
    let status =
        unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes) };
    if status != ERROR_SUCCESS {
        return Err(SysError::msg(format!(
            "GetDisplayConfigBufferSizes failed: {status:?}"
        )));
    }

    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
    let status = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(SysError::msg(format!(
            "QueryDisplayConfig failed: {status:?}"
        )));
    }
    paths.truncate(n_paths as usize);

    let mut out = Vec::new();
    for path in &paths {
        let Some(gdi_name) = source_gdi_name(path) else {
            continue;
        };
        let (monitor_name, connector) = target_name_and_connector(path);
        let r = path.targetInfo.refreshRate;
        out.push(PathConfig {
            gdi_name,
            monitor_name,
            connector,
            refresh_hz: if r.Denominator != 0 {
                Some(r.Numerator as f64 / r.Denominator as f64)
            } else {
                None
            },
        });
    }
    Ok(out)
}

fn source_gdi_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    let mut req = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut req.header) };
    if rc != ERROR_SUCCESS.0 as i32 {
        return None;
    }
    Some(from_wide_nul(&req.viewGdiDeviceName))
}

fn target_name_and_connector(path: &DISPLAYCONFIG_PATH_INFO) -> (Option<String>, Connector) {
    let mut req = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut req.header) };
    let connector = Connector::from_raw(path.targetInfo.outputTechnology.0);
    if rc != ERROR_SUCCESS.0 as i32 {
        return (None, connector);
    }
    let name = from_wide_nul(&req.monitorFriendlyDeviceName);
    (
        if name.is_empty() { None } else { Some(name) },
        Connector::from_raw(req.outputTechnology.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_at_least_one_display() {
        let displays = enumerate().unwrap();
        assert!(
            !displays.is_empty(),
            "a machine running tests has at least one display"
        );
        for d in &displays {
            assert!(!d.gdi_name.is_empty());
            assert!(!d.gpu_name.is_empty(), "GPU name should resolve");
            assert!(d.width > 0 && d.height > 0, "implausible mode for {d:?}");
            assert!(
                d.refresh_hz > 20.0 && d.refresh_hz < 1000.0,
                "implausible refresh {} for {}",
                d.refresh_hz,
                d.gdi_name
            );
        }
    }

    #[test]
    fn exactly_one_primary_display() {
        let displays = enumerate().unwrap();
        assert_eq!(
            displays.iter().filter(|d| d.is_primary).count(),
            1,
            "expected exactly one primary display"
        );
    }

    #[test]
    fn connector_mapping_matches_win32_enum() {
        assert_eq!(Connector::from_raw(5), Connector::Hdmi);
        assert_eq!(Connector::from_raw(10), Connector::DisplayPort);
        assert_eq!(Connector::from_raw(4), Connector::Dvi);
        assert_eq!(Connector::from_raw(0), Connector::Vga);
    }
}
