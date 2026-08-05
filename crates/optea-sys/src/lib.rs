//! Win32 wrappers for OPTEA.
//!
//! This crate is deliberately thin: it exposes typed, testable access to the
//! Windows surfaces OPTEA touches, and nothing else. Policy lives in
//! `optea-core`.

#![cfg(windows)]

pub mod display;
pub mod error;
pub mod foreground;
pub mod gpu;
pub mod paths;
pub mod power;
pub mod registry;
pub mod sysinfo;
pub mod wide;

pub use display::DisplayInfo;
pub use error::{Result, SysError};
pub use gpu::GpuDevice;
pub use power::PowerState;
pub use sysinfo::{CpuInfo, OsInfo};
