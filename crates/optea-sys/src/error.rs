use std::fmt;

/// Errors from the Win32 wrapper layer.
///
/// Every variant carries enough context to name the thing that failed, because
/// these surface directly in `optea doctor` output where "access denied" with no
/// subject is useless to the user.
#[derive(Debug, thiserror::Error)]
pub enum SysError {
    #[error("registry: {op} failed for {path}: {source}")]
    Registry {
        op: &'static str,
        path: String,
        #[source]
        source: windows::core::Error,
    },

    #[error("registry value {path}\\{name} not found")]
    ValueNotFound { path: String, name: String },

    #[error("registry value {path}\\{name} has type {actual}, expected {expected}")]
    ValueType {
        path: String,
        name: String,
        expected: &'static str,
        actual: u32,
    },

    #[error("{api} failed: {source}")]
    Api {
        api: &'static str,
        #[source]
        source: windows::core::Error,
    },

    #[error("{0}")]
    Message(String),
}

impl SysError {
    pub fn api(api: &'static str, source: windows::core::Error) -> Self {
        SysError::Api { api, source }
    }

    pub fn msg(m: impl fmt::Display) -> Self {
        SysError::Message(m.to_string())
    }

    /// True when the failure is "you are not elevated", which callers render as
    /// guidance rather than as a hard error.
    pub fn is_access_denied(&self) -> bool {
        const E_ACCESSDENIED: i32 = -2147024891; // 0x80070005
        const ERROR_ACCESS_DENIED_HR: i32 = -2147024891;
        let code = match self {
            SysError::Registry { source, .. } | SysError::Api { source, .. } => source.code().0,
            _ => return false,
        };
        code == E_ACCESSDENIED || code == ERROR_ACCESS_DENIED_HR
    }
}

pub type Result<T> = std::result::Result<T, SysError>;
