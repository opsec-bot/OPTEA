//! Known-folder resolution.
//!
//! Uses `SHGetKnownFolderPath` rather than `%USERPROFILE%\Documents`, because
//! Documents is frequently redirected — most often into OneDrive. Siege stores
//! `GameSettings.ini` under the *real* Documents folder, so guessing the path
//! from the profile directory finds nothing on a redirected machine.

use crate::error::{Result, SysError};
use std::path::PathBuf;
use windows::core::GUID;
use windows::Win32::Globalization::lstrlenW;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// `FOLDERID_Documents`
const FOLDERID_DOCUMENTS: GUID = GUID::from_u128(0xfdd39ad0_238f_46af_adb4_6c85480369c7);
/// `FOLDERID_ProgramData`
const FOLDERID_PROGRAM_DATA: GUID = GUID::from_u128(0x62ab5d82_fdc1_4dc3_a9dd_070d1d495d97);
/// `FOLDERID_LocalAppData`
const FOLDERID_LOCAL_APP_DATA: GUID = GUID::from_u128(0xf1b32785_6fba_4fcf_9d55_7b8e7f157091);

fn known_folder(id: &GUID) -> Result<PathBuf> {
    let pw = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }
        .map_err(|e| SysError::api("SHGetKnownFolderPath", e))?;
    if pw.is_null() {
        return Err(SysError::msg("SHGetKnownFolderPath returned null"));
    }
    let len = unsafe { lstrlenW(pw) } as usize;
    let slice = unsafe { std::slice::from_raw_parts(pw.0, len) };
    let s = String::from_utf16_lossy(slice);
    unsafe { CoTaskMemFree(Some(pw.0 as *const std::ffi::c_void)) };
    Ok(PathBuf::from(s))
}

/// The user's real Documents folder, following redirection.
pub fn documents() -> Result<PathBuf> {
    known_folder(&FOLDERID_DOCUMENTS)
}

pub fn program_data() -> Result<PathBuf> {
    known_folder(&FOLDERID_PROGRAM_DATA)
}

pub fn local_app_data() -> Result<PathBuf> {
    known_folder(&FOLDERID_LOCAL_APP_DATA)
}

/// Root for OPTEA's own state: snapshots, benchmark results, profiles.
pub fn optea_data_dir() -> Result<PathBuf> {
    Ok(program_data()?.join("OPTEA"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_documents() {
        let p = documents().unwrap();
        assert!(p.is_absolute(), "{p:?} should be absolute");
        assert!(p.exists(), "{p:?} should exist");
    }

    #[test]
    fn resolves_program_data() {
        let p = program_data().unwrap();
        assert!(p.exists(), "{p:?} should exist");
    }

    #[test]
    fn optea_dir_is_under_program_data() {
        let p = optea_data_dir().unwrap();
        assert!(p.ends_with("OPTEA"));
        assert!(p.starts_with(program_data().unwrap()));
    }
}
