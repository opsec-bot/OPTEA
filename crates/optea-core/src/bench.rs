//! Benchmark storage and A/B comparison.
//!
//! Runs are stored under a **label** rather than being compared as they arrive,
//! because a single run cannot distinguish a real change from run-to-run noise.
//! Accumulating several runs per label and comparing distributions is the only
//! honest way to resolve the few-percent effects most tweaks actually produce.
//!
//! The comparison itself lives in [`optea_metrics::stats`], which is allowed to
//! answer "no detectable effect" — and usually should.

use optea_metrics::stats::{self, Effect, FrameSample, Metric, Summary};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Runs per label below which a comparison is refused.
///
/// Two is the arithmetic minimum for any variance estimate; five is where the
/// interval starts being narrow enough to resolve the small effects at stake.
pub const MIN_RUNS: usize = 2;
pub const RECOMMENDED_RUNS: usize = 5;

/// Metrics reported for every comparison, in the order they matter here.
///
/// 1% lows lead because they track perceived smoothness; average FPS is the
/// least interesting number in the set and is included only for context.
pub const REPORTED_METRICS: &[Metric] = &[
    Metric::Low1Fps,
    Metric::Low01Fps,
    Metric::FrameTimeP99Ms,
    Metric::AvgFps,
    Metric::InputLatencyMs,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub label: String,
    pub summary: Summary,
    /// Tweak ids active when this run was taken, for provenance.
    #[serde(default)]
    pub active_tweaks: Vec<String>,
    /// Free-form note, e.g. which map or which benchmark scene.
    #[serde(default)]
    pub note: String,
    /// Fraction of the capture during which the game held focus.
    ///
    /// Recorded per run so an untrustworthy capture stays identifiable after
    /// the fact, rather than blending into a label and skewing its median.
    #[serde(default = "unknown_focus")]
    pub focused_fraction: f64,
}

/// Runs recorded before focus tracking existed have an unknown value; -1
/// distinguishes that from a genuine 0% measurement.
fn unknown_focus() -> f64 {
    -1.0
}

impl Run {
    pub fn focus_known(&self) -> bool {
        self.focused_fraction >= 0.0
    }

    /// True when the game demonstrably held focus for the whole capture.
    pub fn is_trustworthy(&self) -> bool {
        self.focused_fraction >= 0.95
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no runs recorded under label '{0}'")]
    NoSuchLabel(String),

    #[error(
        "label '{label}' has only {have} run(s); at least {MIN_RUNS} are needed to tell a real \
         change from noise. Record more with: optea bench record --label {label}"
    )]
    TooFewRuns { label: String, have: usize },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

type Result<T> = std::result::Result<T, BenchError>;

/// Stored benchmark runs.
pub struct BenchStore {
    dir: PathBuf,
}

impl BenchStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        BenchStore { dir: dir.into() }
    }

    pub fn with_default_dir() -> anyhow::Result<Self> {
        Ok(BenchStore::new(
            optea_sys::paths::optea_data_dir()?.join("benchmarks"),
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Store a run, deriving its summary from captured frames.
    pub fn record(
        &self,
        label: &str,
        frames: &[FrameSample],
        active_tweaks: Vec<String>,
        note: &str,
        focused_fraction: f64,
    ) -> Result<Run> {
        let summary = stats::summarize(frames)
            .ok_or_else(|| BenchError::Other(anyhow::anyhow!("capture held no usable frames")))?;
        self.record_summary_with_focus(label, summary, active_tweaks, note, focused_fraction)
    }

    pub fn record_summary(
        &self,
        label: &str,
        summary: Summary,
        active_tweaks: Vec<String>,
        note: &str,
    ) -> Result<Run> {
        self.record_summary_with_focus(label, summary, active_tweaks, note, 1.0)
    }

    pub fn record_summary_with_focus(
        &self,
        label: &str,
        summary: Summary,
        active_tweaks: Vec<String>,
        note: &str,
        focused_fraction: f64,
    ) -> Result<Run> {
        std::fs::create_dir_all(&self.dir).map_err(|source| BenchError::Io {
            path: self.dir.clone(),
            source,
        })?;

        let run = Run {
            id: timestamp_id(),
            label: label.to_string(),
            summary,
            active_tweaks,
            note: note.to_string(),
            focused_fraction,
        };

        let path = self.dir.join(format!("{}--{}.json", sanitize(label), run.id));
        let json = serde_json::to_string_pretty(&run)
            .map_err(|e| BenchError::Other(anyhow::anyhow!(e)))?;
        std::fs::write(&path, json).map_err(|source| BenchError::Io { path, source })?;
        Ok(run)
    }

    /// All stored runs, oldest first.
    pub fn all_runs(&self) -> Vec<Run> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut runs: Vec<Run> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
            .collect();
        runs.sort_by(|a: &Run, b: &Run| a.id.cmp(&b.id));
        runs
    }

    pub fn runs_for(&self, label: &str) -> Vec<Run> {
        self.all_runs()
            .into_iter()
            .filter(|r| r.label.eq_ignore_ascii_case(label))
            .collect()
    }

    /// Label → run count, for reporting what has been collected so far.
    pub fn labels(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for run in self.all_runs() {
            *out.entry(run.label).or_insert(0) += 1;
        }
        out
    }

    /// Compare two labels across [`REPORTED_METRICS`].
    ///
    /// `seed` fixes the bootstrap resampling so a verdict is reproducible and
    /// reviewable rather than drifting between invocations.
    pub fn compare(
        &self,
        baseline_label: &str,
        variant_label: &str,
        confidence: f64,
        seed: u64,
    ) -> Result<Comparison> {
        let baseline = self.runs_for(baseline_label);
        let variant = self.runs_for(variant_label);

        if baseline.is_empty() {
            return Err(BenchError::NoSuchLabel(baseline_label.into()));
        }
        if variant.is_empty() {
            return Err(BenchError::NoSuchLabel(variant_label.into()));
        }
        if baseline.len() < MIN_RUNS {
            return Err(BenchError::TooFewRuns {
                label: baseline_label.into(),
                have: baseline.len(),
            });
        }
        if variant.len() < MIN_RUNS {
            return Err(BenchError::TooFewRuns {
                label: variant_label.into(),
                have: variant.len(),
            });
        }

        let effects = REPORTED_METRICS
            .iter()
            .filter_map(|m| {
                // A metric absent from either side (input latency, typically) is
                // skipped rather than defaulted to zero.
                let b: Vec<f64> = baseline.iter().filter_map(|r| m.extract(&r.summary)).collect();
                let v: Vec<f64> = variant.iter().filter_map(|r| m.extract(&r.summary)).collect();
                if b.len() < MIN_RUNS || v.len() < MIN_RUNS {
                    return None;
                }
                stats::compare(*m, &b, &v, confidence, seed)
            })
            .collect();

        // A run captured while the game was backgrounded measures the engine's
        // idle throttle, not the game. Surfaced rather than silently averaged in.
        let untrustworthy: Vec<String> = baseline
            .iter()
            .chain(variant.iter())
            .filter(|r| r.focus_known() && !r.is_trustworthy())
            .map(|r| format!("{} ({:.0}% focused)", r.id, r.focused_fraction * 100.0))
            .collect();

        Ok(Comparison {
            baseline_label: baseline_label.to_string(),
            variant_label: variant_label.to_string(),
            baseline_runs: baseline.len(),
            variant_runs: variant.len(),
            confidence,
            effects,
            underpowered: baseline.len() < RECOMMENDED_RUNS || variant.len() < RECOMMENDED_RUNS,
            untrustworthy,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub baseline_label: String,
    pub variant_label: String,
    pub baseline_runs: usize,
    pub variant_runs: usize,
    pub confidence: f64,
    pub effects: Vec<Effect>,
    /// True when there are enough runs to compare, but few enough that only a
    /// large effect could be resolved.
    pub underpowered: bool,
    /// Runs captured while the game did not hold focus, and are therefore
    /// measuring a background throttle rather than gameplay.
    pub untrustworthy: Vec<String>,
}

impl Comparison {
    /// The metric this project cares about most.
    pub fn headline(&self) -> Option<&Effect> {
        self.effects
            .iter()
            .find(|e| e.metric == Metric::Low1Fps)
            .or_else(|| self.effects.first())
    }

    /// True when no metric showed a detectable change — the common outcome.
    pub fn all_inconclusive(&self) -> bool {
        !self.effects.is_empty()
            && self
                .effects
                .iter()
                .all(|e| e.verdict == stats::Verdict::NoDetectableEffect)
    }

    /// One-line conclusion in the project's own terms.
    pub fn conclusion(&self) -> String {
        if !self.untrustworthy.is_empty() {
            return format!(
                "{} run(s) were captured while the game was not in focus. Games throttle \
                 rendering in the background, so this comparison is not usable — re-record them \
                 with the game focused.",
                self.untrustworthy.len()
            );
        }
        if self.effects.is_empty() {
            return "no comparable metrics between these labels".into();
        }
        if self.all_inconclusive() {
            return format!(
                "No detectable effect on any metric. On this machine, '{}' is not measurably \
                 different from '{}'.",
                self.variant_label, self.baseline_label
            );
        }
        match self.headline() {
            Some(e) => e.describe(),
            None => "inconclusive".into(),
        }
    }
}

fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect()
}

fn timestamp_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (y, m, d) = crate::engine::civil_from_days_pub(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{:03}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        now.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> BenchStore {
        let dir = std::env::temp_dir().join(format!("optea-bench-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        BenchStore::new(dir)
    }

    /// A summary with a given 1% low, other fields kept self-consistent.
    fn summary(low_1: f64, avg: f64) -> Summary {
        Summary {
            frames: 1000,
            duration_s: 10.0,
            avg_fps: avg,
            low_1_fps: low_1,
            low_01_fps: low_1 * 0.9,
            frame_time_p50_ms: 1000.0 / avg,
            frame_time_p99_ms: 1000.0 / low_1,
            gpu_busy_mean_ms: Some(2.0),
            input_latency_p50_ms: Some(20.0),
        }
    }

    fn record_many(s: &BenchStore, label: &str, lows: &[f64]) {
        for (i, low) in lows.iter().enumerate() {
            s.record_summary(label, summary(*low, 100.0 + i as f64), vec![], "")
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn records_and_lists_runs_by_label() {
        let s = store("record");
        record_many(&s, "baseline", &[60.0, 61.0, 59.0]);
        record_many(&s, "variant", &[70.0, 71.0]);

        assert_eq!(s.runs_for("baseline").len(), 3);
        assert_eq!(s.runs_for("variant").len(), 2);
        assert_eq!(s.labels()["baseline"], 3);

        // Label matching ignores case, so 'Baseline' is not a second bucket.
        assert_eq!(s.runs_for("BASELINE").len(), 3);
    }

    #[test]
    fn refuses_to_compare_a_single_run() {
        // The core discipline: one run per side carries no variance information.
        let s = store("single");
        record_many(&s, "a", &[60.0]);
        record_many(&s, "b", &[90.0]);

        let err = s.compare("a", "b", 0.95, 1).unwrap_err();
        assert!(matches!(err, BenchError::TooFewRuns { .. }), "{err:?}");
        assert!(err.to_string().contains("tell a real change from noise"));
    }

    #[test]
    fn unknown_label_is_reported_clearly() {
        let s = store("unknown");
        record_many(&s, "a", &[60.0, 61.0]);
        assert!(matches!(
            s.compare("a", "nope", 0.95, 1).unwrap_err(),
            BenchError::NoSuchLabel(_)
        ));
    }

    #[test]
    fn identical_labels_show_no_detectable_effect() {
        // Comparing a condition against a copy of itself must not manufacture
        // a winner. This is the guard the whole harness rests on.
        let s = store("identical");
        record_many(&s, "a", &[60.0, 62.0, 58.0, 61.0, 59.0]);
        record_many(&s, "b", &[61.0, 59.0, 60.0, 58.0, 62.0]);

        let cmp = s.compare("a", "b", 0.95, 42).unwrap();
        assert!(
            cmp.all_inconclusive(),
            "expected no effect, got: {}",
            cmp.conclusion()
        );
        assert!(cmp.conclusion().contains("No detectable effect"));
    }

    #[test]
    fn a_large_real_improvement_is_detected() {
        let s = store("improvement");
        record_many(&s, "before", &[40.0, 41.0, 39.0, 40.5, 39.5]);
        record_many(&s, "after", &[70.0, 71.0, 69.0, 70.5, 69.5]);

        let cmp = s.compare("before", "after", 0.95, 42).unwrap();
        assert!(!cmp.all_inconclusive());
        let head = cmp.headline().unwrap();
        assert_eq!(head.metric, Metric::Low1Fps);
        assert_eq!(head.verdict, stats::Verdict::Improvement);
        assert!(head.delta > 0.0);
    }

    #[test]
    fn flags_a_comparison_with_too_few_runs_to_be_powerful() {
        let s = store("underpowered");
        record_many(&s, "a", &[60.0, 61.0]);
        record_many(&s, "b", &[62.0, 63.0]);

        let cmp = s.compare("a", "b", 0.95, 7).unwrap();
        assert!(
            cmp.underpowered,
            "2 runs per side should be marked underpowered"
        );
        assert_eq!(cmp.baseline_runs, 2);
    }

    #[test]
    fn enough_runs_is_not_flagged_underpowered() {
        let s = store("powered");
        record_many(&s, "a", &[60.0, 61.0, 59.0, 60.5, 59.5]);
        record_many(&s, "b", &[62.0, 63.0, 61.0, 62.5, 61.5]);
        assert!(!s.compare("a", "b", 0.95, 7).unwrap().underpowered);
    }

    #[test]
    fn comparison_is_reproducible_for_a_seed() {
        let s = store("repro");
        record_many(&s, "a", &[60.0, 64.0, 58.0, 61.0, 62.0]);
        record_many(&s, "b", &[63.0, 66.0, 61.0, 64.0, 65.0]);

        let x = s.compare("a", "b", 0.95, 99).unwrap();
        let y = s.compare("a", "b", 0.95, 99).unwrap();
        assert_eq!(x.headline().unwrap().ci_low, y.headline().unwrap().ci_low);
        assert_eq!(x.headline().unwrap().verdict, y.headline().unwrap().verdict);
    }

    #[test]
    fn one_percent_low_leads_the_report() {
        let s = store("ordering");
        record_many(&s, "a", &[60.0, 61.0, 59.0]);
        record_many(&s, "b", &[60.5, 61.5, 59.5]);
        let cmp = s.compare("a", "b", 0.95, 3).unwrap();
        assert_eq!(
            cmp.effects.first().unwrap().metric,
            Metric::Low1Fps,
            "1% low must lead; average FPS hides the stutters that matter"
        );
    }

    #[test]
    fn runs_survive_a_reload_from_disk() {
        let dir = std::env::temp_dir().join("optea-bench-reload");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let s = BenchStore::new(&dir);
            record_many(&s, "persisted", &[55.0, 56.0]);
        }
        let reopened = BenchStore::new(&dir);
        assert_eq!(reopened.runs_for("persisted").len(), 2);
    }

    #[test]
    fn an_unfocused_run_invalidates_a_comparison() {
        // The failure mode this guard exists for: a backgrounded game throttles
        // to ~30 FPS, which looks like a plausible measurement rather than an
        // obvious error, and would be attributed to whatever tweak was tested.
        let s = store("focus");
        for low in [60.0, 61.0] {
            s.record_summary_with_focus("focused", summary(low, 100.0), vec![], "", 1.0)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        for low in [15.0, 14.0] {
            s.record_summary_with_focus("backgrounded", summary(low, 29.0), vec![], "", 0.0)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let cmp = s.compare("focused", "backgrounded", 0.95, 42).unwrap();
        assert_eq!(cmp.untrustworthy.len(), 2);
        assert!(
            cmp.conclusion().contains("not in focus"),
            "must refuse rather than report a huge fake regression: {}",
            cmp.conclusion()
        );
    }

    #[test]
    fn fully_focused_runs_are_not_flagged() {
        let s = store("focus-clean");
        record_many(&s, "a", &[60.0, 61.0, 59.0]);
        record_many(&s, "b", &[60.5, 61.5, 59.5]);
        assert!(s.compare("a", "b", 0.95, 42).unwrap().untrustworthy.is_empty());
    }

    #[test]
    fn runs_predating_focus_tracking_are_not_condemned() {
        // Absent focus data is unknown, not zero; such a run must not be
        // reported as though it were measured at 0% focus.
        let run = Run {
            id: "20260101-000000-000".into(),
            label: "old".into(),
            summary: summary(60.0, 100.0),
            active_tweaks: vec![],
            note: String::new(),
            focused_fraction: unknown_focus(),
        };
        assert!(!run.focus_known());
        assert!(!run.is_trustworthy());
    }

    #[test]
    fn labels_with_awkward_characters_are_stored_safely() {
        let s = store("sanitize");
        s.record_summary("window mode: 0/exclusive", summary(60.0, 100.0), vec![], "")
            .unwrap();
        assert_eq!(s.runs_for("window mode: 0/exclusive").len(), 1);
    }
}
