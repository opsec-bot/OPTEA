//! Frame-time statistics and effect estimation.
//!
//! This module exists to stop OPTEA from fooling itself. Most Windows "gaming
//! tweaks" do nothing measurable, and a naive harness that reports
//! `after - before` will happily attribute run-to-run noise to whatever it just
//! toggled. Every comparison here goes through a bootstrap confidence interval
//! and is allowed to conclude [`Verdict::NoDetectableEffect`] — which, for most
//! tweaks in the catalog, is the correct answer.

use serde::{Deserialize, Serialize};

/// One presented frame.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FrameSample {
    /// Interval since the previous present, in milliseconds.
    pub frame_time_ms: f64,
    /// Time the GPU spent actively working on this frame, when available.
    pub gpu_busy_ms: Option<f64>,
    /// Input-to-photon latency, when the game and driver report it.
    pub input_latency_ms: Option<f64>,
}

impl FrameSample {
    pub fn new(frame_time_ms: f64) -> Self {
        FrameSample {
            frame_time_ms,
            gpu_busy_ms: None,
            input_latency_ms: None,
        }
    }
}

/// Aggregate statistics for a single capture run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub frames: usize,
    pub duration_s: f64,
    pub avg_fps: f64,
    /// Mean of the slowest 1% of frames, expressed as FPS. This is the metric
    /// that tracks perceived smoothness; average FPS hides exactly the stutters
    /// a competitive player notices.
    pub low_1_fps: f64,
    /// Mean of the slowest 0.1% of frames, as FPS.
    pub low_01_fps: f64,
    pub frame_time_p50_ms: f64,
    pub frame_time_p99_ms: f64,
    pub gpu_busy_mean_ms: Option<f64>,
    pub input_latency_p50_ms: Option<f64>,
}

/// Which metric an A/B comparison is being judged on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metric {
    AvgFps,
    Low1Fps,
    Low01Fps,
    FrameTimeP99Ms,
    InputLatencyMs,
}

impl Metric {
    pub fn label(&self) -> &'static str {
        match self {
            Metric::AvgFps => "avg FPS",
            Metric::Low1Fps => "1% low FPS",
            Metric::Low01Fps => "0.1% low FPS",
            Metric::FrameTimeP99Ms => "frametime p99 (ms)",
            Metric::InputLatencyMs => "input latency (ms)",
        }
    }

    /// True when a larger value is better (FPS), false when smaller is better
    /// (latency, frame time). Getting this backwards would invert every verdict.
    pub fn higher_is_better(&self) -> bool {
        match self {
            Metric::AvgFps | Metric::Low1Fps | Metric::Low01Fps => true,
            Metric::FrameTimeP99Ms | Metric::InputLatencyMs => false,
        }
    }

    pub fn extract(&self, s: &Summary) -> Option<f64> {
        match self {
            Metric::AvgFps => Some(s.avg_fps),
            Metric::Low1Fps => Some(s.low_1_fps),
            Metric::Low01Fps => Some(s.low_01_fps),
            Metric::FrameTimeP99Ms => Some(s.frame_time_p99_ms),
            Metric::InputLatencyMs => s.input_latency_p50_ms,
        }
    }
}

/// Summarise one capture run. Returns `None` for an empty capture.
pub fn summarize(frames: &[FrameSample]) -> Option<Summary> {
    if frames.is_empty() {
        return None;
    }

    let mut times: Vec<f64> = frames
        .iter()
        .map(|f| f.frame_time_ms)
        .filter(|t| t.is_finite() && *t > 0.0)
        .collect();
    if times.is_empty() {
        return None;
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let total_ms: f64 = times.iter().sum();
    let n = times.len();

    let gpu: Vec<f64> = frames.iter().filter_map(|f| f.gpu_busy_ms).collect();
    let mut latency: Vec<f64> = frames.iter().filter_map(|f| f.input_latency_ms).collect();
    latency.sort_by(|a, b| a.partial_cmp(b).unwrap());

    Some(Summary {
        frames: n,
        duration_s: total_ms / 1000.0,
        avg_fps: n as f64 / (total_ms / 1000.0),
        low_1_fps: slowest_fraction_as_fps(&times, 0.01),
        low_01_fps: slowest_fraction_as_fps(&times, 0.001),
        frame_time_p50_ms: percentile_sorted(&times, 0.50),
        frame_time_p99_ms: percentile_sorted(&times, 0.99),
        gpu_busy_mean_ms: if gpu.is_empty() {
            None
        } else {
            Some(gpu.iter().sum::<f64>() / gpu.len() as f64)
        },
        input_latency_p50_ms: if latency.is_empty() {
            None
        } else {
            Some(percentile_sorted(&latency, 0.50))
        },
    })
}

/// Mean of the slowest `fraction` of frames, converted to FPS.
///
/// `times` must be sorted ascending, so the slowest frames are at the end.
/// Always includes at least one frame, so short captures still produce a number.
fn slowest_fraction_as_fps(times: &[f64], fraction: f64) -> f64 {
    let count = ((times.len() as f64 * fraction).ceil() as usize).clamp(1, times.len());
    let slowest = &times[times.len() - count..];
    let mean_ms = slowest.iter().sum::<f64>() / count as f64;
    1000.0 / mean_ms
}

/// Linear-interpolated percentile of an ascending-sorted slice.
fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
    }
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    percentile_sorted(&v, 0.5)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Confidence interval lies entirely on the better side of zero.
    Improvement,
    /// Confidence interval lies entirely on the worse side of zero.
    Regression,
    /// The interval straddles zero. The honest and most common answer.
    NoDetectableEffect,
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Improvement => "improvement",
            Verdict::Regression => "regression",
            Verdict::NoDetectableEffect => "no detectable effect",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    pub metric: Metric,
    pub baseline_median: f64,
    pub variant_median: f64,
    /// `variant - baseline`, in the metric's own units.
    pub delta: f64,
    pub delta_pct: f64,
    /// Confidence interval on `delta`.
    pub ci_low: f64,
    pub ci_high: f64,
    pub confidence: f64,
    pub verdict: Verdict,
    pub baseline_runs: usize,
    pub variant_runs: usize,
}

impl Effect {
    /// Human-readable one-liner for the results table.
    pub fn describe(&self) -> String {
        match self.verdict {
            Verdict::NoDetectableEffect => format!(
                "{}: no detectable effect ({:+.1}%, CI [{:+.1}, {:+.1}])",
                self.metric.label(),
                self.delta_pct,
                self.ci_low,
                self.ci_high
            ),
            _ => format!(
                "{}: {} {:+.1}% (CI [{:+.1}, {:+.1}])",
                self.metric.label(),
                self.verdict.label(),
                self.delta_pct,
                self.ci_low,
                self.ci_high
            ),
        }
    }
}

/// Number of bootstrap resamples. High enough that percentile estimates are
/// stable run to run, low enough to stay instant on a few dozen samples.
pub const BOOTSTRAP_ITERATIONS: usize = 10_000;

/// Compare two sets of runs and decide whether the difference is real.
///
/// `baseline` and `variant` are per-run values of the same metric (one entry per
/// benchmark run, not per frame). Requires at least two runs on each side —
/// a single run per condition cannot distinguish signal from noise, so this
/// returns `None` rather than pretending otherwise.
pub fn compare(
    metric: Metric,
    baseline: &[f64],
    variant: &[f64],
    confidence: f64,
    seed: u64,
) -> Option<Effect> {
    if baseline.len() < 2 || variant.len() < 2 {
        return None;
    }

    let base_med = median(baseline);
    let var_med = median(variant);
    let delta = var_med - base_med;

    let mut rng = Rng::new(seed);
    let mut deltas = Vec::with_capacity(BOOTSTRAP_ITERATIONS);
    for _ in 0..BOOTSTRAP_ITERATIONS {
        let b = resample(baseline, &mut rng);
        let v = resample(variant, &mut rng);
        deltas.push(median(&v) - median(&b));
    }
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let alpha = (1.0 - confidence) / 2.0;
    let ci_low = percentile_sorted(&deltas, alpha);
    let ci_high = percentile_sorted(&deltas, 1.0 - alpha);

    // A verdict is only issued when the whole interval sits on one side of zero.
    let verdict = if ci_low > 0.0 && ci_high > 0.0 {
        if metric.higher_is_better() {
            Verdict::Improvement
        } else {
            Verdict::Regression
        }
    } else if ci_low < 0.0 && ci_high < 0.0 {
        if metric.higher_is_better() {
            Verdict::Regression
        } else {
            Verdict::Improvement
        }
    } else {
        Verdict::NoDetectableEffect
    };

    Some(Effect {
        metric,
        baseline_median: base_med,
        variant_median: var_med,
        delta,
        delta_pct: if base_med != 0.0 {
            delta / base_med * 100.0
        } else {
            0.0
        },
        ci_low,
        ci_high,
        confidence,
        verdict,
        baseline_runs: baseline.len(),
        variant_runs: variant.len(),
    })
}

fn resample(values: &[f64], rng: &mut Rng) -> Vec<f64> {
    (0..values.len())
        .map(|_| values[rng.next_below(values.len() as u64) as usize])
        .collect()
}

/// SplitMix64. Deterministic given a seed, so benchmark verdicts are
/// reproducible and reviewable rather than shifting between runs.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(times: &[f64]) -> Vec<FrameSample> {
        times.iter().map(|t| FrameSample::new(*t)).collect()
    }

    /// Deterministic pseudo-noise around a mean, for building synthetic runs.
    fn noisy_runs(mean: f64, spread: f64, n: usize, seed: u64) -> Vec<f64> {
        let mut rng = Rng::new(seed);
        (0..n)
            .map(|_| {
                let u = rng.next_u64() as f64 / u64::MAX as f64; // [0,1)
                mean + (u - 0.5) * 2.0 * spread
            })
            .collect()
    }

    #[test]
    fn avg_fps_matches_uniform_frame_times() {
        // 100 frames at exactly 10 ms is exactly 100 FPS.
        let s = summarize(&frames(&[10.0; 100])).unwrap();
        assert!((s.avg_fps - 100.0).abs() < 1e-9, "got {}", s.avg_fps);
        assert!((s.duration_s - 1.0).abs() < 1e-9);
        assert_eq!(s.frames, 100);
    }

    #[test]
    fn one_percent_low_reflects_the_worst_frames() {
        // 99 fast frames (10 ms) and one 50 ms stutter.
        let mut times = vec![10.0; 99];
        times.push(50.0);
        let s = summarize(&frames(&times)).unwrap();

        // The 1% low must isolate the stutter: 1000/50 = 20 FPS.
        assert!(
            (s.low_1_fps - 20.0).abs() < 1e-9,
            "1% low should be 20, got {}",
            s.low_1_fps
        );
        // Average FPS barely notices it — which is exactly why we track lows.
        assert!(s.avg_fps > 90.0, "avg fps was {}", s.avg_fps);
    }

    #[test]
    fn low_metrics_never_exceed_average() {
        let times: Vec<f64> = (0..500).map(|i| 8.0 + (i % 7) as f64).collect();
        let s = summarize(&frames(&times)).unwrap();
        assert!(s.low_1_fps <= s.avg_fps);
        assert!(s.low_01_fps <= s.low_1_fps);
    }

    #[test]
    fn empty_capture_summarizes_to_none() {
        assert!(summarize(&[]).is_none());
        // Non-finite / non-positive frame times are discarded, not counted.
        assert!(summarize(&frames(&[0.0, -1.0, f64::NAN])).is_none());
    }

    #[test]
    fn percentiles_interpolate() {
        let v = vec![0.0, 10.0];
        assert!((percentile_sorted(&v, 0.5) - 5.0).abs() < 1e-9);
        assert!((percentile_sorted(&v, 0.0) - 0.0).abs() < 1e-9);
        assert!((percentile_sorted(&v, 1.0) - 10.0).abs() < 1e-9);
    }

    // ---- The verification that matters most --------------------------------

    #[test]
    fn identical_distributions_report_no_effect() {
        // Plan verification step 3: comparing a condition against itself must
        // NOT produce a verdict. If this fails, every downstream result is junk.
        let a = noisy_runs(100.0, 3.0, 8, 1);
        let b = noisy_runs(100.0, 3.0, 8, 2);
        let e = compare(Metric::Low1Fps, &a, &b, 0.95, 42).unwrap();
        assert_eq!(
            e.verdict,
            Verdict::NoDetectableEffect,
            "identical distributions produced {:?} ({})",
            e.verdict,
            e.describe()
        );
        assert!(e.ci_low <= 0.0 && e.ci_high >= 0.0, "CI must straddle zero");
    }

    #[test]
    fn same_samples_on_both_sides_report_no_effect() {
        let a = noisy_runs(75.0, 2.0, 10, 7);
        let e = compare(Metric::AvgFps, &a, &a, 0.95, 99).unwrap();
        assert_eq!(e.verdict, Verdict::NoDetectableEffect);
        assert!((e.delta).abs() < 1e-9);
    }

    #[test]
    fn large_real_improvement_is_detected() {
        let base = noisy_runs(60.0, 1.0, 8, 3);
        let variant = noisy_runs(75.0, 1.0, 8, 4);
        let e = compare(Metric::Low1Fps, &base, &variant, 0.95, 42).unwrap();
        assert_eq!(e.verdict, Verdict::Improvement, "{}", e.describe());
        assert!(e.delta > 0.0);
        assert!(e.delta_pct > 15.0, "delta_pct was {}", e.delta_pct);
    }

    #[test]
    fn large_regression_is_detected() {
        let base = noisy_runs(100.0, 1.0, 8, 5);
        let variant = noisy_runs(80.0, 1.0, 8, 6);
        let e = compare(Metric::AvgFps, &base, &variant, 0.95, 42).unwrap();
        assert_eq!(e.verdict, Verdict::Regression, "{}", e.describe());
        assert!(e.delta < 0.0);
    }

    #[test]
    fn lower_is_better_metrics_invert_the_verdict() {
        // Latency dropping from 20 ms to 12 ms is an improvement, not a regression.
        let base = noisy_runs(20.0, 0.5, 8, 11);
        let variant = noisy_runs(12.0, 0.5, 8, 12);
        let e = compare(Metric::InputLatencyMs, &base, &variant, 0.95, 42).unwrap();
        assert_eq!(e.verdict, Verdict::Improvement, "{}", e.describe());
        assert!(e.delta < 0.0, "latency should have gone down");
    }

    #[test]
    fn tiny_differences_stay_inside_the_noise() {
        // A 1% shift with 3% run-to-run spread is not resolvable at this N, and
        // must not be reported as a win.
        let base = noisy_runs(100.0, 3.0, 5, 21);
        let variant = noisy_runs(101.0, 3.0, 5, 22);
        let e = compare(Metric::Low1Fps, &base, &variant, 0.95, 42).unwrap();
        assert_eq!(
            e.verdict,
            Verdict::NoDetectableEffect,
            "a 1% shift under 3% noise was reported as {:?}",
            e.verdict
        );
    }

    #[test]
    fn refuses_to_compare_single_runs() {
        // One run per side carries no information about variance.
        assert!(compare(Metric::AvgFps, &[100.0], &[110.0], 0.95, 1).is_none());
        assert!(compare(Metric::AvgFps, &[100.0, 101.0], &[110.0], 0.95, 1).is_none());
    }

    #[test]
    fn results_are_deterministic_for_a_given_seed() {
        let base = noisy_runs(90.0, 4.0, 6, 31);
        let variant = noisy_runs(93.0, 4.0, 6, 32);
        let a = compare(Metric::Low1Fps, &base, &variant, 0.95, 7).unwrap();
        let b = compare(Metric::Low1Fps, &base, &variant, 0.95, 7).unwrap();
        assert_eq!(a.ci_low, b.ci_low);
        assert_eq!(a.ci_high, b.ci_high);
        assert_eq!(a.verdict, b.verdict);
    }

    #[test]
    fn wider_confidence_widens_the_interval() {
        let base = noisy_runs(100.0, 5.0, 8, 41);
        let variant = noisy_runs(104.0, 5.0, 8, 42);
        let narrow = compare(Metric::AvgFps, &base, &variant, 0.80, 5).unwrap();
        let wide = compare(Metric::AvgFps, &base, &variant, 0.99, 5).unwrap();
        assert!(
            wide.ci_high - wide.ci_low > narrow.ci_high - narrow.ci_low,
            "99% interval should be wider than 80%"
        );
    }

    #[test]
    fn metric_direction_is_explicit() {
        assert!(Metric::AvgFps.higher_is_better());
        assert!(Metric::Low1Fps.higher_is_better());
        assert!(!Metric::InputLatencyMs.higher_is_better());
        assert!(!Metric::FrameTimeP99Ms.higher_is_better());
    }
}
