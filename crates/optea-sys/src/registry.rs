//! Typed registry access.
//!
//! The critical design point: [`read`] returns `Ok(None)` when a value does not
//! exist, distinct from `Ok(Some(Dword(0)))`. Many tweaks *create* a value that
//! was previously absent, and reverting one of those means **deleting** it, not
//! writing zero. Collapsing those two states is the classic way an optimizer
//! leaves a machine permanently altered after the user asks it to undo.

use crate::error::{Result, SysError};
use crate::wide::{from_wide_multi, from_wide_nul, to_wide_multi, wide};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCreateKeyExW, RegCloseKey, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY,
    HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_WRITE,
    REG_BINARY, REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ, REG_OPTION_NON_VOLATILE, REG_QWORD,
    REG_SZ, REG_VALUE_TYPE, RRF_NOEXPAND, RRF_RT_ANY,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Root {
    Hklm,
    Hkcu,
    Hkcr,
    Hku,
}

impl Root {
    fn hkey(self) -> HKEY {
        match self {
            Root::Hklm => HKEY_LOCAL_MACHINE,
            Root::Hkcu => HKEY_CURRENT_USER,
            Root::Hkcr => HKEY_CLASSES_ROOT,
            Root::Hku => HKEY_USERS,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Root::Hklm => "HKLM",
            Root::Hkcu => "HKCU",
            Root::Hkcr => "HKCR",
            Root::Hku => "HKU",
        }
    }
}

/// A registry value, preserving its exact type for round-trip fidelity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RegValue {
    Dword(u32),
    Qword(u64),
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    Binary(Vec<u8>),
    /// A value that exists but holds REG_NONE.
    None,
}

impl RegValue {
    fn type_code(&self) -> REG_VALUE_TYPE {
        match self {
            RegValue::Dword(_) => REG_DWORD,
            RegValue::Qword(_) => REG_QWORD,
            RegValue::Sz(_) => REG_SZ,
            RegValue::ExpandSz(_) => REG_EXPAND_SZ,
            RegValue::MultiSz(_) => REG_MULTI_SZ,
            RegValue::Binary(_) => REG_BINARY,
            RegValue::None => REG_VALUE_TYPE(0),
        }
    }

    pub fn as_dword(&self) -> Option<u32> {
        match self {
            RegValue::Dword(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            RegValue::Sz(s) | RegValue::ExpandSz(s) => Some(s),
            _ => None,
        }
    }
}

/// Identifies a single registry value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegKey {
    pub root: Root,
    pub subkey: String,
    pub name: String,
}

impl RegKey {
    pub fn new(root: Root, subkey: impl Into<String>, name: impl Into<String>) -> Self {
        RegKey {
            root,
            subkey: subkey.into(),
            name: name.into(),
        }
    }

    pub fn hklm(subkey: impl Into<String>, name: impl Into<String>) -> Self {
        RegKey::new(Root::Hklm, subkey, name)
    }

    pub fn hkcu(subkey: impl Into<String>, name: impl Into<String>) -> Self {
        RegKey::new(Root::Hkcu, subkey, name)
    }

    pub fn display_path(&self) -> String {
        format!("{}\\{}", self.root.as_str(), self.subkey)
    }
}

/// Read a value. `Ok(None)` means the value (or its key) does not exist.
pub fn read(key: &RegKey) -> Result<Option<RegValue>> {
    let subkey = wide(&key.subkey);
    let name = wide(&key.name);

    // First call sizes the buffer. RRF_NOEXPAND keeps REG_EXPAND_SZ verbatim so
    // a capture/restore cycle does not silently bake in expanded paths.
    let mut ty = REG_VALUE_TYPE(0);
    let mut cb: u32 = 0;
    let status = unsafe {
        RegGetValueW(
            key.root.hkey(),
            subkey.as_pcwstr(),
            name.as_pcwstr(),
            RRF_RT_ANY | RRF_NOEXPAND,
            Some(&mut ty),
            None,
            Some(&mut cb),
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
        return Err(SysError::Registry {
            op: "RegGetValueW(size)",
            path: format!("{}\\{}", key.display_path(), key.name),
            source: windows::core::Error::from(status.to_hresult()),
        });
    }

    let mut buf = vec![0u8; cb as usize];
    let mut cb2 = cb;
    let status = unsafe {
        RegGetValueW(
            key.root.hkey(),
            subkey.as_pcwstr(),
            name.as_pcwstr(),
            RRF_RT_ANY | RRF_NOEXPAND,
            Some(&mut ty),
            if cb == 0 {
                None
            } else {
                Some(buf.as_mut_ptr() as *mut c_void)
            },
            Some(&mut cb2),
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(SysError::Registry {
            op: "RegGetValueW(read)",
            path: format!("{}\\{}", key.display_path(), key.name),
            source: windows::core::Error::from(status.to_hresult()),
        });
    }
    buf.truncate(cb2 as usize);

    Ok(Some(decode(ty, &buf)))
}

fn decode(ty: REG_VALUE_TYPE, buf: &[u8]) -> RegValue {
    let as_u16 = |b: &[u8]| -> Vec<u16> {
        b.chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    };

    match ty {
        REG_DWORD if buf.len() >= 4 => {
            RegValue::Dword(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
        }
        REG_QWORD if buf.len() >= 8 => RegValue::Qword(u64::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ])),
        REG_SZ => RegValue::Sz(from_wide_nul(&as_u16(buf))),
        REG_EXPAND_SZ => RegValue::ExpandSz(from_wide_nul(&as_u16(buf))),
        REG_MULTI_SZ => RegValue::MultiSz(from_wide_multi(&as_u16(buf))),
        _ if buf.is_empty() => RegValue::None,
        _ => RegValue::Binary(buf.to_vec()),
    }
}

/// Write a value, creating the key if it does not exist.
pub fn write(key: &RegKey, value: &RegValue) -> Result<()> {
    ensure_key(key)?;

    let subkey = wide(&key.subkey);
    let name = wide(&key.name);

    // Owned encodings must outlive the call, hence the explicit bindings.
    let (ptr, len): (*const c_void, u32) = match value {
        RegValue::Dword(v) => (v as *const u32 as *const c_void, 4),
        RegValue::Qword(v) => (v as *const u64 as *const c_void, 8),
        RegValue::Sz(s) | RegValue::ExpandSz(s) => {
            let w = wide(s);
            return set_raw(
                key,
                &subkey,
                &name,
                value.type_code(),
                w.as_pcwstr().0 as *const c_void,
                (s.encode_utf16().count() as u32 + 1) * 2,
            );
        }
        RegValue::MultiSz(items) => {
            let w = to_wide_multi(items);
            return set_raw(
                key,
                &subkey,
                &name,
                REG_MULTI_SZ,
                w.as_ptr() as *const c_void,
                (w.len() * 2) as u32,
            );
        }
        RegValue::Binary(b) => (b.as_ptr() as *const c_void, b.len() as u32),
        RegValue::None => (std::ptr::null(), 0),
    };

    set_raw(key, &subkey, &name, value.type_code(), ptr, len)
}

fn set_raw(
    key: &RegKey,
    subkey: &crate::wide::WideString,
    name: &crate::wide::WideString,
    ty: REG_VALUE_TYPE,
    ptr: *const c_void,
    len: u32,
) -> Result<()> {
    let status = unsafe {
        RegSetKeyValueW(
            key.root.hkey(),
            subkey.as_pcwstr(),
            name.as_pcwstr(),
            ty.0,
            if ptr.is_null() { None } else { Some(ptr) },
            len,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(SysError::Registry {
            op: "RegSetKeyValueW",
            path: format!("{}\\{}", key.display_path(), key.name),
            source: windows::core::Error::from(status.to_hresult()),
        });
    }
    Ok(())
}

/// Delete a value. Succeeds if it is already absent.
pub fn delete(key: &RegKey) -> Result<()> {
    let subkey = wide(&key.subkey);
    let name = wide(&key.name);
    let status =
        unsafe { RegDeleteKeyValueW(key.root.hkey(), subkey.as_pcwstr(), name.as_pcwstr()) };
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    Err(SysError::Registry {
        op: "RegDeleteKeyValueW",
        path: format!("{}\\{}", key.display_path(), key.name),
        source: windows::core::Error::from(status.to_hresult()),
    })
}

/// Restore a captured state: `Some` writes the value, `None` deletes it.
///
/// This is the inverse of [`read`] and the reason capture must record absence.
pub fn restore(key: &RegKey, captured: &Option<RegValue>) -> Result<()> {
    match captured {
        Some(v) => write(key, v),
        None => delete(key),
    }
}

fn ensure_key(key: &RegKey) -> Result<()> {
    let subkey = wide(&key.subkey);
    let mut hkey = HKEY::default();
    let status = unsafe {
        RegCreateKeyExW(
            key.root.hkey(),
            subkey.as_pcwstr(),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(SysError::Registry {
            op: "RegCreateKeyExW",
            path: key.display_path(),
            source: windows::core::Error::from(status.to_hresult()),
        });
    }
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_value_reads_as_none() {
        let key = RegKey::hklm(
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion",
            "OpteaDefinitelyNotARealValue",
        );
        assert_eq!(read(&key).unwrap(), None);
    }

    #[test]
    fn reads_a_known_system_value() {
        // CurrentBuild is present on every supported Windows version.
        let key = RegKey::hklm(
            "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
            "CurrentBuild",
        );
        let v = read(&key).unwrap().expect("CurrentBuild should exist");
        assert!(v.as_str().is_some(), "expected REG_SZ, got {v:?}");
    }

    #[test]
    fn absent_key_reads_as_none() {
        let key = RegKey::hklm("SOFTWARE\\OpteaNoSuchKey\\Nested", "Whatever");
        assert_eq!(read(&key).unwrap(), None);
    }

    /// Scratch key under HKCU so the round-trip tests need no elevation.
    fn scratch(name: &str) -> RegKey {
        RegKey::hkcu("Software\\OPTEA\\TestScratch", name)
    }

    #[test]
    fn every_value_type_round_trips() {
        let cases = vec![
            ("dword", RegValue::Dword(0x26)),
            ("dword_zero", RegValue::Dword(0)),
            ("qword", RegValue::Qword(0xDEAD_BEEF_CAFE)),
            ("sz", RegValue::Sz("hello world".into())),
            ("sz_empty", RegValue::Sz(String::new())),
            ("expand", RegValue::ExpandSz("%SystemRoot%\\system32".into())),
            (
                "multi",
                RegValue::MultiSz(vec!["alpha".into(), "beta".into(), "gamma".into()]),
            ),
            ("binary", RegValue::Binary(vec![0, 1, 2, 250, 255])),
        ];

        for (name, value) in cases {
            let key = scratch(name);
            write(&key, &value).unwrap();
            let got = read(&key).unwrap();
            assert_eq!(got, Some(value.clone()), "round-trip failed for {name}");
            delete(&key).unwrap();
        }
    }

    #[test]
    fn expand_sz_is_not_expanded_on_read() {
        // If RRF_NOEXPAND were dropped, capture would store the expanded path and
        // restore would write a literal C:\Windows into a value that should stay
        // relocatable.
        let key = scratch("noexpand");
        let raw = "%SystemRoot%\\system32";
        write(&key, &RegValue::ExpandSz(raw.into())).unwrap();
        assert_eq!(read(&key).unwrap(), Some(RegValue::ExpandSz(raw.into())));
        delete(&key).unwrap();
    }

    #[test]
    fn restore_of_absent_capture_deletes_the_value() {
        // The property the whole revert story rests on: a tweak that CREATES a
        // value must, on revert, leave no value behind.
        let key = scratch("was_absent");
        let captured = read(&key).unwrap();
        assert_eq!(captured, None, "precondition: scratch value must not exist");

        write(&key, &RegValue::Dword(1)).unwrap();
        assert_eq!(read(&key).unwrap(), Some(RegValue::Dword(1)));

        restore(&key, &captured).unwrap();
        assert_eq!(
            read(&key).unwrap(),
            None,
            "restore must delete a value that did not previously exist"
        );
    }

    #[test]
    fn restore_of_present_capture_rewrites_the_original() {
        let key = scratch("was_present");
        write(&key, &RegValue::Dword(2)).unwrap();

        let captured = read(&key).unwrap();
        write(&key, &RegValue::Dword(0x26)).unwrap();
        restore(&key, &captured).unwrap();

        assert_eq!(read(&key).unwrap(), Some(RegValue::Dword(2)));
        delete(&key).unwrap();
    }

    #[test]
    fn delete_is_idempotent() {
        let key = scratch("never_existed");
        delete(&key).unwrap();
        delete(&key).unwrap();
    }
}
