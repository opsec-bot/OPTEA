//! OS identity, CPU topology, and elevation state.

use crate::error::{Result, SysError};
use crate::registry::{self, RegKey};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

/// The build at which Windows 11 begins. This is the only reliable discriminator:
/// `ProductName` still reads "Windows 10 ..." on many Windows 11 installs.
pub const WIN11_MIN_BUILD: u32 = 22000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsInfo {
    pub product_name: String,
    /// Feature update label, e.g. "22H2". Absent on older builds.
    pub display_version: Option<String>,
    pub build: u32,
    /// Update Build Revision — the fourth version component.
    pub ubr: Option<u32>,
}

impl OsInfo {
    pub fn query() -> Result<Self> {
        let s = |name: &str| -> Option<String> {
            registry::read(&RegKey::hklm(CURRENT_VERSION, name))
                .ok()
                .flatten()
                .and_then(|v| v.as_str().map(str::to_owned))
        };
        let d = |name: &str| -> Option<u32> {
            registry::read(&RegKey::hklm(CURRENT_VERSION, name))
                .ok()
                .flatten()
                .and_then(|v| v.as_dword())
        };

        let build = s("CurrentBuild")
            .or_else(|| s("CurrentBuildNumber"))
            .and_then(|b| b.parse::<u32>().ok())
            .ok_or_else(|| SysError::msg("could not determine Windows build number"))?;

        Ok(OsInfo {
            product_name: s("ProductName").unwrap_or_else(|| "Windows".into()),
            display_version: s("DisplayVersion"),
            build,
            ubr: d("UBR"),
        })
    }

    pub fn is_windows_11(&self) -> bool {
        self.build >= WIN11_MIN_BUILD
    }

    /// Windows 10 2004 (build 19041) made timer-resolution requests per-process.
    pub fn has_per_process_timer_resolution(&self) -> bool {
        self.build >= 19041
    }

    /// `GlobalTimerResolutionRequests` is read by the kernel only on Windows 11.
    /// On Windows 10 there is no supported way to restore system-wide timer
    /// resolution, which makes the whole tweak inapplicable rather than merely
    /// ineffective.
    pub fn supports_global_timer_resolution(&self) -> bool {
        self.is_windows_11()
    }

    pub fn version_string(&self) -> String {
        let mut s = self.product_name.clone();
        if let Some(dv) = &self.display_version {
            s.push_str(&format!(" {dv}"));
        }
        s.push_str(&format!(" (build {}", self.build));
        if let Some(ubr) = self.ubr {
            s.push_str(&format!(".{ubr}"));
        }
        s.push(')');
        s
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfo {
    pub name: String,
    pub physical_cores: u32,
    pub logical_processors: u32,
}

impl CpuInfo {
    pub fn query() -> Result<Self> {
        let name = registry::read(&RegKey::hklm(
            r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "ProcessorNameString",
        ))?
        .and_then(|v| v.as_str().map(|s| s.trim().to_owned()))
        .unwrap_or_else(|| "Unknown CPU".into());

        let (physical_cores, logical_processors) = core_counts()?;
        Ok(CpuInfo {
            name,
            physical_cores,
            logical_processors,
        })
    }

    /// True when the CPU exposes more logical processors than physical cores.
    pub fn smt_enabled(&self) -> bool {
        self.logical_processors > self.physical_cores
    }
}

fn core_counts() -> Result<(u32, u32)> {
    let mut len: u32 = 0;
    // First call fails with ERROR_INSUFFICIENT_BUFFER and reports the size.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len) };
    if len == 0 {
        return Err(SysError::msg(
            "GetLogicalProcessorInformationEx reported zero size",
        ));
    }

    let mut buf = vec![0u8; len as usize];
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX),
            &mut len,
        )
    }
    .map_err(|e| SysError::api("GetLogicalProcessorInformationEx", e))?;

    let mut physical = 0u32;
    let mut logical = 0u32;
    let mut offset = 0usize;
    while offset + std::mem::size_of::<u32>() * 2 <= len as usize {
        let rec = unsafe {
            &*(buf.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)
        };
        let size = rec.Size as usize;
        if size == 0 {
            break;
        }
        physical += 1;
        // One GROUP_AFFINITY per group this core spans; count set mask bits.
        let proc_info = unsafe { &rec.Anonymous.Processor };
        for i in 0..proc_info.GroupCount as usize {
            logical += proc_info.GroupMask[i].Mask.count_ones();
        }
        offset += size;
    }

    Ok((physical, logical))
}

/// True when the current process is running with an elevated token.
pub fn is_elevated() -> Result<bool> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|e| SysError::api("OpenProcessToken", e))?;

    let mut elevation = TOKEN_ELEVATION::default();
    let mut ret_len = 0u32;
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    result.map_err(|e| SysError::api("GetTokenInformation", e))?;

    Ok(elevation.TokenIsElevated != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_os_info() {
        let os = OsInfo::query().unwrap();
        assert!(os.build > 10000, "implausible build {}", os.build);
        // Consistency: the Win11 predicate must agree with the build threshold.
        assert_eq!(os.is_windows_11(), os.build >= WIN11_MIN_BUILD);
        assert_eq!(os.supports_global_timer_resolution(), os.is_windows_11());
    }

    #[test]
    fn reads_cpu_info() {
        let cpu = CpuInfo::query().unwrap();
        assert!(!cpu.name.is_empty());
        assert!(cpu.physical_cores >= 1, "no physical cores counted");
        assert!(
            cpu.logical_processors >= cpu.physical_cores,
            "logical ({}) < physical ({})",
            cpu.logical_processors,
            cpu.physical_cores
        );
    }

    #[test]
    fn logical_count_matches_std_parallelism() {
        let cpu = CpuInfo::query().unwrap();
        let std_count = std::thread::available_parallelism().unwrap().get() as u32;
        assert_eq!(
            cpu.logical_processors, std_count,
            "GROUP_AFFINITY mask walk disagrees with std"
        );
    }

    #[test]
    fn elevation_query_succeeds() {
        is_elevated().unwrap();
    }
}
