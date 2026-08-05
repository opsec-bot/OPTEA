//! PresentMon capture and the statistics that decide whether a tweak did anything.

pub mod ffi;
pub mod presentmon;
pub mod stats;

pub use stats::{compare, summarize, Effect, FrameSample, Metric, Summary, Verdict};
