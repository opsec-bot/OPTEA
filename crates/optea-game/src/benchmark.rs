//! Reading Siege's own benchmark reports.
//!
//! The game writes a `.ini` summary and an `.html` report to
//! `Documents\My Games\Rainbow Six - Siege\Benchmark\` after each run. This is a
//! better primary source than a timed external capture for three reasons:
//!
//! * it covers the **whole** run, with no window to align against a scene whose
//!   length is not known in advance;
//! * the scene is deterministic, which is the entire basis for A/B comparison;
//! * the HTML carries the engine's own **CPU and GPU time per sample**, which
//!   external frame timing cannot separate.
//!
//! It does not replace PresentMon: the series is downsampled (a few hundred
//! samples for several thousand frames), so true 1% lows still need per-frame
//! data. The two are complementary, and agreeing with each other is itself
//! evidence that neither is wrong.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum BenchmarkError {
    #[error("no benchmark reports found in {dir}. Run the in-game benchmark first.")]
    NoReports { dir: PathBuf },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is missing the expected key '{key}'")]
    MissingKey { path: PathBuf, key: &'static str },
}

type Result<T> = std::result::Result<T, BenchmarkError>;

/// One completed in-game benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub ini_path: PathBuf,
    pub html_path: Option<PathBuf>,
    /// Timestamp parsed from the file name, e.g. `2026.08.04-21.30.46`.
    pub stamp: String,

    pub avg_fps: f64,
    pub lowest_fps: f64,
    pub highest_fps: f64,
    pub frame_count: u64,
    /// Total rendered seconds, excluding loading.
    pub duration_s: f64,
    pub smallest_frame_time_ms: f64,
    pub largest_frame_time_ms: f64,
    pub loading_time_ms: Option<f64>,

    /// Per-sample CPU time in ms, from the HTML report. Downsampled.
    #[serde(default)]
    pub cpu_times_ms: Vec<f64>,
    /// Per-sample GPU time in ms, from the HTML report. Downsampled.
    #[serde(default)]
    pub gpu_times_ms: Vec<f64>,
}

impl BenchmarkReport {
    /// Fraction of samples where CPU time exceeded GPU time.
    ///
    /// The engine's own verdict on which side is the constraint.
    pub fn cpu_bound_fraction(&self) -> Option<f64> {
        if self.cpu_times_ms.is_empty() || self.cpu_times_ms.len() != self.gpu_times_ms.len() {
            return None;
        }
        let n = self.cpu_times_ms.len();
        let over = self
            .cpu_times_ms
            .iter()
            .zip(&self.gpu_times_ms)
            .filter(|(c, g)| c > g)
            .count();
        Some(over as f64 / n as f64)
    }

    /// Same, ignoring the first `skip` samples, which cover level loading and
    /// are not representative of the rendered scene.
    pub fn cpu_bound_fraction_after(&self, skip: usize) -> Option<f64> {
        if self.cpu_times_ms.len() <= skip || self.cpu_times_ms.len() != self.gpu_times_ms.len() {
            return None;
        }
        let c = &self.cpu_times_ms[skip..];
        let g = &self.gpu_times_ms[skip..];
        let over = c.iter().zip(g).filter(|(c, g)| c > g).count();
        Some(over as f64 / c.len() as f64)
    }

    pub fn cpu_percentile(&self, q: f64) -> Option<f64> {
        percentile(&self.cpu_times_ms, q)
    }

    pub fn gpu_percentile(&self, q: f64) -> Option<f64> {
        percentile(&self.gpu_times_ms, q)
    }

    /// True when the slow tail is dominated by CPU rather than GPU time.
    ///
    /// This is what decides whether lowering graphics settings can help the
    /// stutters a player actually feels.
    pub fn tail_is_cpu_bound(&self) -> Option<bool> {
        Some(self.cpu_percentile(0.95)? > self.gpu_percentile(0.95)? * 1.5)
    }

    /// Mean frame time implied by the reported average FPS.
    pub fn mean_frame_time_ms(&self) -> f64 {
        if self.avg_fps > 0.0 {
            1000.0 / self.avg_fps
        } else {
            f64::NAN
        }
    }
}

fn percentile(v: &[f64], q: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((s.len() - 1) as f64 * q).round() as usize;
    Some(s[idx.min(s.len() - 1)])
}

/// Default report directory under the user's Documents folder.
pub fn report_dir() -> std::result::Result<PathBuf, optea_sys::SysError> {
    Ok(optea_sys::paths::documents()?
        .join("My Games")
        .join("Rainbow Six - Siege")
        .join("Benchmark"))
}

/// All reports in `dir`, newest first.
pub fn list_reports(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Err(BenchmarkError::NoReports {
            dir: dir.to_path_buf(),
        });
    }
    let entries = std::fs::read_dir(dir).map_err(|source| BenchmarkError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut inis: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "ini")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("Benchmark-"))
        })
        .collect();
    // File names embed a sortable timestamp, so lexical order is chronological.
    inis.sort();
    inis.reverse();
    if inis.is_empty() {
        return Err(BenchmarkError::NoReports {
            dir: dir.to_path_buf(),
        });
    }
    Ok(inis)
}

/// The most recent report.
pub fn latest_report(dir: &Path) -> Result<BenchmarkReport> {
    let newest = list_reports(dir)?.remove(0);
    parse_report(&newest)
}

/// Parse a report from its `.ini`, pulling in the sibling `.html` if present.
pub fn parse_report(ini_path: &Path) -> Result<BenchmarkReport> {
    let text = std::fs::read_to_string(ini_path).map_err(|source| BenchmarkError::Io {
        path: ini_path.to_path_buf(),
        source,
    })?;

    let get = |key: &'static str| -> Option<f64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .and_then(|v| v.trim().parse().ok())
    };
    let need = |key: &'static str| -> Result<f64> {
        get(key).ok_or(BenchmarkError::MissingKey {
            path: ini_path.to_path_buf(),
            key,
        })
    };

    let stamp = ini_path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("Benchmark-"))
        .unwrap_or_default()
        .to_string();

    let html_path = {
        let candidate = ini_path.with_extension("html");
        candidate.is_file().then_some(candidate)
    };
    let (cpu_times_ms, gpu_times_ms) = match &html_path {
        Some(p) => parse_html_series(p).unwrap_or_default(),
        None => (Vec::new(), Vec::new()),
    };

    Ok(BenchmarkReport {
        avg_fps: need("Fps")?,
        lowest_fps: get("LowestFps").unwrap_or(f64::NAN),
        highest_fps: get("HighestFps").unwrap_or(f64::NAN),
        frame_count: get("RawFrameCounts").unwrap_or(0.0) as u64,
        duration_s: get("RawFrameTimes").unwrap_or(f64::NAN),
        // The engine reports these in seconds.
        smallest_frame_time_ms: get("RawSmallestFrameTime").unwrap_or(f64::NAN) * 1000.0,
        largest_frame_time_ms: get("RawLargestFrameTime").unwrap_or(f64::NAN) * 1000.0,
        loading_time_ms: get("LoadingTime"),
        ini_path: ini_path.to_path_buf(),
        html_path,
        stamp,
        cpu_times_ms,
        gpu_times_ms,
    })
}

/// Pull the CPU and GPU time series out of the HTML report.
fn parse_html_series(path: &Path) -> Option<(Vec<f64>, Vec<f64>)> {
    let text = std::fs::read_to_string(path).ok()?;
    Some((
        extract_array(&text, "dataPointsCPU_time"),
        extract_array(&text, "dataPointsGPU_time"),
    ))
}

/// Read `var <name> = [ ... ];` from the report's inline script.
///
/// The report declares each series several times — commented-out placeholder
/// lines such as `//var dataPointsCPU_time = randomDataSet(limit, 5, 16);` sit
/// above the real assignment. Matching the first occurrence of the name lands
/// inside a comment and then scans forward to some unrelated `[`, which is how
/// this silently produced an empty series against the real file.
///
/// So a candidate only counts when it is a genuine array assignment on a line
/// that is not commented out.
pub fn extract_array(text: &str, name: &str) -> Vec<f64> {
    let needle = format!("var {name}");
    let mut from = 0usize;

    while let Some(rel) = text[from..].find(&needle) {
        let start = from + rel;
        from = start + needle.len();

        // Skip the declaration if its line is commented out.
        let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        if text[line_start..start].trim_start().starts_with("//") {
            continue;
        }

        // Require `= [` (whitespace tolerated) rather than any later bracket,
        // so a call like `= randomDataSet(...)` is not mistaken for an array.
        let after = &text[from..];
        let Some(eq) = after.find('=') else { continue };
        if after[..eq].chars().any(|c| !c.is_whitespace()) {
            continue;
        }
        let tail = &after[eq + 1..];
        let open_rel = tail.len() - tail.trim_start().len();
        if !tail[open_rel..].starts_with('[') {
            continue;
        }

        let body = &tail[open_rel + 1..];
        let Some(close) = body.find(']') else { continue };
        return body[..close]
            .split(',')
            .filter_map(|s| s.trim().parse::<f64>().ok())
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from a real report.
    const SAMPLE_INI: &str = "[]\r\n\
LowestFps=3.546272\r\n\
Fps=104.632416\r\n\
HighestFps=183.257584\r\n\
RawFrameTimes=77.796158\r\n\
RawFrameCounts=8140\r\n\
RawSmallestFrameTime=0.005457\r\n\
RawLargestFrameTime=0.281986\r\n\
RawGraphicsFlipDuration=0.000000\r\n\
\r\n\
[Overall]\r\n\
LoadingTime=15414\r\n";

    fn write_fixture(name: &str, ini: &str, html: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("optea-bm-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Benchmark-2026.08.04-21.30.46.ini");
        std::fs::write(&p, ini).unwrap();
        if let Some(h) = html {
            std::fs::write(p.with_extension("html"), h).unwrap();
        }
        p
    }

    #[test]
    fn parses_the_real_ini_shape() {
        let p = write_fixture("ini", SAMPLE_INI, None);
        let r = parse_report(&p).unwrap();

        assert!((r.avg_fps - 104.632416).abs() < 1e-6);
        assert!((r.lowest_fps - 3.546272).abs() < 1e-6);
        assert!((r.highest_fps - 183.257584).abs() < 1e-6);
        assert_eq!(r.frame_count, 8140);
        assert!((r.duration_s - 77.796158).abs() < 1e-6);
        assert_eq!(r.loading_time_ms, Some(15414.0));
        assert_eq!(r.stamp, "2026.08.04-21.30.46");
    }

    #[test]
    fn frame_times_are_converted_from_seconds_to_ms() {
        // The engine writes seconds; reporting 0.28 "ms" would understate a
        // 282 ms hitch by three orders of magnitude.
        let p = write_fixture("units", SAMPLE_INI, None);
        let r = parse_report(&p).unwrap();
        assert!((r.smallest_frame_time_ms - 5.457).abs() < 1e-3);
        assert!((r.largest_frame_time_ms - 281.986).abs() < 1e-3);
    }

    #[test]
    fn reported_fps_is_consistent_with_frames_over_duration() {
        let p = write_fixture("consistency", SAMPLE_INI, None);
        let r = parse_report(&p).unwrap();
        let derived = r.frame_count as f64 / r.duration_s;
        assert!(
            (derived - r.avg_fps).abs() < 0.5,
            "frames/duration = {derived:.2} but Fps = {:.2}",
            r.avg_fps
        );
    }

    #[test]
    fn extracts_series_from_html() {
        let html = "junk var dataPointsCPU_time = [34.20,8.26,7.99];more \
                    var dataPointsGPU_time = [8.64,8.37,7.41]; tail";
        let p = write_fixture("html", SAMPLE_INI, Some(html));
        let r = parse_report(&p).unwrap();
        assert_eq!(r.cpu_times_ms, vec![34.20, 8.26, 7.99]);
        assert_eq!(r.gpu_times_ms, vec![8.64, 8.37, 7.41]);
        assert!(r.html_path.is_some());
    }

    #[test]
    fn ignores_commented_out_declarations() {
        // Exactly the shape of the real report: a commented placeholder above
        // the genuine assignment. Matching the first occurrence yields nothing.
        let html = "\
    // CPU TIME\n\
    //var dataPointsCPU_time = randomDataSet(limit, 5, 16);\n\
    //var dataPointsGPU_time = randomDataSet(limit, 5, 16);\n\
    var dataPointsCPU_time = [11.5,12.5,13.5];\n\
    var dataPointsGPU_time = [4.5,5.5,6.5];\n";
        assert_eq!(
            extract_array(html, "dataPointsCPU_time"),
            vec![11.5, 12.5, 13.5]
        );
        assert_eq!(
            extract_array(html, "dataPointsGPU_time"),
            vec![4.5, 5.5, 6.5]
        );
    }

    #[test]
    fn ignores_a_function_call_assignment() {
        let html = "var dataPointsCPU_time = randomDataSet(limit, 5, 16);\n\
                    var other = [1,2,3];\n";
        assert!(
            extract_array(html, "dataPointsCPU_time").is_empty(),
            "must not scan forward into an unrelated array"
        );
    }

    #[test]
    fn extracts_the_real_series_on_this_machine_if_present() {
        // The regression this guards: parsing the actual report must yield a
        // non-empty, equal-length pair of series.
        let Ok(dir) = report_dir() else { return };
        let Ok(r) = latest_report(&dir) else { return };
        if r.html_path.is_none() {
            return;
        }
        assert!(
            !r.cpu_times_ms.is_empty(),
            "the real report has a CPU series; extraction returned nothing"
        );
        assert_eq!(r.cpu_times_ms.len(), r.gpu_times_ms.len());
        // Frame times in milliseconds, so implausible values mean a mis-parse.
        assert!(r.cpu_times_ms.iter().all(|v| *v > 0.0 && *v < 10_000.0));
    }

    #[test]
    fn detects_a_cpu_bound_workload() {
        let html = "var dataPointsCPU_time = [30,30,30,8,9];\
                    var dataPointsGPU_time = [7,7,7,7,7];";
        let p = write_fixture("cpubound", SAMPLE_INI, Some(html));
        let r = parse_report(&p).unwrap();

        assert_eq!(r.cpu_bound_fraction(), Some(1.0));
        assert_eq!(r.tail_is_cpu_bound(), Some(true));
    }

    #[test]
    fn detects_a_gpu_bound_workload() {
        let html = "var dataPointsCPU_time = [4,4,4,4,4];\
                    var dataPointsGPU_time = [16,16,16,16,16];";
        let p = write_fixture("gpubound", SAMPLE_INI, Some(html));
        let r = parse_report(&p).unwrap();

        assert_eq!(r.cpu_bound_fraction(), Some(0.0));
        assert_eq!(r.tail_is_cpu_bound(), Some(false));
    }

    #[test]
    fn loading_samples_can_be_excluded() {
        // Early samples cover level loading and would otherwise skew the verdict.
        let html = "var dataPointsCPU_time = [34,34,5,5,5,5];\
                    var dataPointsGPU_time = [8,8,9,9,9,9];";
        let p = write_fixture("skip", SAMPLE_INI, Some(html));
        let r = parse_report(&p).unwrap();

        assert!((r.cpu_bound_fraction().unwrap() - 2.0 / 6.0).abs() < 1e-9);
        assert_eq!(r.cpu_bound_fraction_after(2), Some(0.0));
    }

    #[test]
    fn missing_html_is_not_fatal() {
        let p = write_fixture("nohtml", SAMPLE_INI, None);
        let r = parse_report(&p).unwrap();
        assert!(r.cpu_times_ms.is_empty());
        assert_eq!(r.cpu_bound_fraction(), None);
        assert_eq!(r.tail_is_cpu_bound(), None);
    }

    #[test]
    fn missing_required_key_is_reported() {
        let p = write_fixture("missing", "[]\r\nLowestFps=1.0\r\n", None);
        assert!(matches!(
            parse_report(&p).unwrap_err(),
            BenchmarkError::MissingKey { key: "Fps", .. }
        ));
    }

    #[test]
    fn empty_report_dir_is_reported_clearly() {
        let dir = std::env::temp_dir().join("optea-bm-empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(
            list_reports(&dir).unwrap_err(),
            BenchmarkError::NoReports { .. }
        ));
    }

    #[test]
    fn reports_are_listed_newest_first() {
        let dir = std::env::temp_dir().join("optea-bm-order");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for stamp in ["2026.08.04-10.00.00", "2026.08.04-21.30.46", "2026.08.03-09.00.00"] {
            std::fs::write(dir.join(format!("Benchmark-{stamp}.ini")), SAMPLE_INI).unwrap();
        }
        let reports = list_reports(&dir).unwrap();
        assert!(reports[0].to_string_lossy().contains("2026.08.04-21.30.46"));
        assert!(reports[2].to_string_lossy().contains("2026.08.03"));
    }

    #[test]
    fn parses_the_real_report_on_this_machine_if_present() {
        let Ok(dir) = report_dir() else { return };
        let Ok(report) = latest_report(&dir) else {
            return;
        };
        assert!(report.avg_fps > 0.0 && report.avg_fps < 1000.0);
        assert!(report.frame_count > 0);
        // If the HTML was found, the two series must line up.
        if !report.cpu_times_ms.is_empty() {
            assert_eq!(report.cpu_times_ms.len(), report.gpu_times_ms.len());
        }
    }
}
