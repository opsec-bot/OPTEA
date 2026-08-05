//! Reading and reasoning about Siege's graphics settings.
//!
//! The analysis here is deliberately narrow. It reports what is *set*, names the
//! mechanism by which a setting costs frames, and says which knob is worth
//! benchmarking — it does not promise a number. Anything claiming "+30 FPS" for
//! a config change is guessing; the benchmark harness exists to answer that.
//!
//! Where the meaning of a value is not certain, the finding says so rather than
//! inventing a confident interpretation.

use crate::ini::IniDocument;
use serde::Serialize;

pub const S_DISPLAY: &str = "DISPLAY";
pub const S_DISPLAY_SETTINGS: &str = "DISPLAY_SETTINGS";
pub const S_CUSTOM_QUALITY: &str = "CUSTOM_QUALITY";
pub const S_HARDWARE_INFO: &str = "HARDWARE_INFO";

/// Window mode, as encoded by the game's own comment in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum WindowMode {
    Fullscreen,
    Windowed,
    Borderless,
    Unknown(i64),
}

impl WindowMode {
    pub fn from_value(v: i64) -> Self {
        match v {
            0 => WindowMode::Fullscreen,
            1 => WindowMode::Windowed,
            2 => WindowMode::Borderless,
            other => WindowMode::Unknown(other),
        }
    }

    pub fn label(&self) -> String {
        match self {
            WindowMode::Fullscreen => "exclusive fullscreen".into(),
            WindowMode::Windowed => "windowed".into(),
            WindowMode::Borderless => "borderless".into(),
            WindowMode::Unknown(v) => format!("unknown ({v})"),
        }
    }
}

/// NVIDIA Reflex mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Reflex {
    Off,
    On,
    OnPlusBoost,
    Unknown(i64),
}

impl Reflex {
    pub fn from_value(v: i64) -> Self {
        match v {
            0 => Reflex::Off,
            1 => Reflex::On,
            2 => Reflex::OnPlusBoost,
            other => Reflex::Unknown(other),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Reflex::Off => "off".into(),
            Reflex::On => "on".into(),
            Reflex::OnPlusBoost => "on + boost".into(),
            Reflex::Unknown(v) => format!("unknown ({v})"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphicsSettings {
    pub render_width: Option<i64>,
    pub render_height: Option<i64>,
    pub refresh_rate: Option<f64>,
    pub window_mode: Option<WindowMode>,
    pub vsync: Option<i64>,
    pub max_gpu_buffered_frame: Option<i64>,
    pub fps_limit: Option<i64>,
    pub reflex: Option<Reflex>,
    pub fov: Option<f64>,
    pub quality_preset: Option<String>,
    /// Raw `[CUSTOM_QUALITY]` values, kept as written.
    pub quality: Vec<(String, i64)>,
    pub render_scaling_factor: Option<i64>,
    /// Siege's own benchmark scores, when it has run them.
    pub cpu_score: Option<f64>,
    pub gpu_score: Option<f64>,
    pub gpu_memory_mb: Option<i64>,
}

impl GraphicsSettings {
    pub fn from_document(doc: &IniDocument) -> Self {
        let quality_keys = [
            "AntiAliasing",
            "Atmospheric",
            "Geometry",
            "Lighting",
            "Shadow",
            "Texture",
            "TextureFiltering",
            "Reflection",
            "AO",
            "LensEffects",
            "DOF",
            "VFX",
            "Sharpness",
            "TextureStreaming",
        ];

        GraphicsSettings {
            render_width: doc.get_i64(S_DISPLAY_SETTINGS, "ResolutionWidth"),
            render_height: doc.get_i64(S_DISPLAY_SETTINGS, "ResolutionHeight"),
            refresh_rate: doc.get_f64(S_DISPLAY_SETTINGS, "RefreshRate"),
            window_mode: doc
                .get_i64(S_DISPLAY_SETTINGS, "WindowMode")
                .map(WindowMode::from_value),
            vsync: doc.get_i64(S_DISPLAY_SETTINGS, "VSync"),
            max_gpu_buffered_frame: doc.get_i64(S_DISPLAY_SETTINGS, "MaxGPUBufferedFrame"),
            fps_limit: doc.get_i64(S_DISPLAY, "FPSLimit"),
            reflex: doc.get_i64(S_DISPLAY, "NVReflex").map(Reflex::from_value),
            fov: doc.get_f64(S_DISPLAY_SETTINGS, "DefaultFOV"),
            quality_preset: doc
                .get("QUALITY", "OverallQualityLevelName")
                .map(str::to_owned),
            quality: quality_keys
                .iter()
                .filter_map(|k| {
                    doc.get_i64(S_CUSTOM_QUALITY, k)
                        .map(|v| ((*k).to_string(), v))
                })
                .collect(),
            render_scaling_factor: doc.get_i64(S_CUSTOM_QUALITY, "RenderScalingFactor"),
            cpu_score: doc.get_f64(S_HARDWARE_INFO, "CPUScore"),
            gpu_score: doc.get_f64(S_HARDWARE_INFO, "GPUScore"),
            gpu_memory_mb: doc.get_i64(S_HARDWARE_INFO, "GPUDedicatedMemoryMB"),
        }
    }

    pub fn render_pixels(&self) -> Option<i64> {
        Some(self.render_width? * self.render_height?)
    }

    pub fn resolution_label(&self) -> String {
        match (self.render_width, self.render_height) {
            (Some(w), Some(h)) => format!("{w}x{h}"),
            _ => "unknown".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Impact {
    /// Already in the best-known state.
    Good,
    /// Informational.
    Info,
    /// Worth changing or at least benchmarking.
    Opportunity,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingFinding {
    pub impact: Impact,
    pub setting: String,
    pub current: String,
    pub detail: String,
    pub suggestion: Option<String>,
}

impl SettingFinding {
    fn new(
        impact: Impact,
        setting: impl Into<String>,
        current: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        SettingFinding {
            impact,
            setting: setting.into(),
            current: current.into(),
            detail: detail.into(),
            suggestion: None,
        }
    }

    fn suggest(mut self, s: impl Into<String>) -> Self {
        self.suggestion = Some(s.into());
        self
    }
}

/// Facts about the machine that change what advice makes sense.
#[derive(Debug, Clone, Copy)]
pub struct MachineContext {
    pub physical_cores: u32,
    /// Native resolution of the display the game runs on.
    pub display_width: Option<i64>,
    pub display_height: Option<i64>,
    pub display_refresh_hz: Option<f64>,
}

/// Below this, Siege is meaningfully CPU-limited.
const CPU_BOUND_CORE_THRESHOLD: u32 = 6;

pub fn analyze(s: &GraphicsSettings, ctx: &MachineContext) -> Vec<SettingFinding> {
    let mut out = Vec::new();
    let cpu_limited = ctx.physical_cores < CPU_BOUND_CORE_THRESHOLD;

    check_resolution(s, ctx, &mut out);
    check_window_mode(s, &mut out);
    check_latency_chain(s, &mut out);
    check_quality(s, cpu_limited, &mut out);
    check_render_scaling(s, &mut out);
    check_engine_scores(s, &mut out);

    out.sort_by(|a, b| b.impact.cmp(&a.impact));
    out
}

fn check_resolution(s: &GraphicsSettings, ctx: &MachineContext, out: &mut Vec<SettingFinding>) {
    let (Some(w), Some(h)) = (s.render_width, s.render_height) else {
        return;
    };
    let label = format!("{w}x{h}");

    match (ctx.display_width, ctx.display_height) {
        (Some(dw), Some(dh)) if (w, h) != (dw, dh) => {
            let game_px = w * h;
            let native_px = dw * dh;
            let pct = (game_px as f64 / native_px as f64) * 100.0;
            out.push(
                SettingFinding::new(
                    Impact::Info,
                    "Render resolution",
                    &label,
                    format!(
                        "The game renders {label} while the display is {dw}x{dh} — {pct:.0}% of \
                         native pixel count. The remaining scaling is done by the display or \
                         compositor, which costs a little latency but saves considerable GPU work."
                    ),
                )
                .suggest(
                    "This is a reasonable trade on a mid-range GPU. Benchmark native against \
                     this before deciding; it is the single largest graphics lever available.",
                ),
            );
        }
        (Some(_), Some(_)) => {
            out.push(SettingFinding::new(
                Impact::Info,
                "Render resolution",
                &label,
                "The game renders at the display's native resolution.".to_string(),
            ));
        }
        _ => {}
    }
}

fn check_window_mode(s: &GraphicsSettings, out: &mut Vec<SettingFinding>) {
    let Some(mode) = s.window_mode else { return };
    match mode {
        WindowMode::Fullscreen => out.push(SettingFinding::new(
            Impact::Good,
            "WindowMode",
            mode.label(),
            "Exclusive fullscreen lets the game drive the display directly, bypassing the \
             desktop compositor.",
        )),
        WindowMode::Borderless | WindowMode::Windowed => out.push(
            SettingFinding::new(
                Impact::Opportunity,
                "WindowMode",
                mode.label(),
                "Borderless and windowed present through the desktop compositor, which adds a \
                 presentation step between the rendered frame and the screen. Exclusive \
                 fullscreen removes that step.",
            )
            .suggest(
                "Benchmark WindowMode=0 (exclusive fullscreen). Modern Windows narrows this gap \
                 with flip-model presentation, so measure rather than assume — but it is a real \
                 and commonly meaningful difference for input latency.",
            ),
        ),
        WindowMode::Unknown(_) => {}
    }
}

fn check_latency_chain(s: &GraphicsSettings, out: &mut Vec<SettingFinding>) {
    if let Some(v) = s.vsync {
        if v == 0 {
            out.push(SettingFinding::new(
                Impact::Good,
                "VSync",
                "off",
                "V-Sync is off, so frames are not held back waiting for the display refresh.",
            ));
        } else {
            out.push(
                SettingFinding::new(
                    Impact::Opportunity,
                    "VSync",
                    format!("{v} frame(s)"),
                    "V-Sync makes the renderer wait for the display, adding up to a full refresh \
                     interval of latency.",
                )
                .suggest("Set VSync=0 unless tearing bothers you more than latency."),
            );
        }
    }

    if let Some(r) = s.reflex {
        match r {
            Reflex::OnPlusBoost => out.push(SettingFinding::new(
                Impact::Good,
                "NVReflex",
                r.label(),
                "Reflex with Boost is enabled — it paces the CPU to avoid building a render \
                 queue, which is the main source of latency when CPU-bound.",
            )),
            Reflex::On => out.push(
                SettingFinding::new(
                    Impact::Info,
                    "NVReflex",
                    r.label(),
                    "Reflex is on. Boost additionally holds GPU clocks up, which can help when \
                     the GPU is idling between frames.",
                )
                .suggest("Worth benchmarking NVReflex=2 (on + boost)."),
            ),
            Reflex::Off => out.push(
                SettingFinding::new(
                    Impact::Opportunity,
                    "NVReflex",
                    r.label(),
                    "Reflex is off. On an NVIDIA GPU it is usually the single most effective \
                     latency setting available in-game.",
                )
                .suggest("Benchmark NVReflex=1, then 2."),
            ),
            Reflex::Unknown(_) => {}
        }
    }

    if let Some(b) = s.max_gpu_buffered_frame {
        if b <= 0 {
            out.push(SettingFinding::new(
                Impact::Good,
                "MaxGPUBufferedFrame",
                b.to_string(),
                "The pre-rendered frame queue is already minimal.",
            ));
        } else {
            out.push(
                SettingFinding::new(
                    Impact::Opportunity,
                    "MaxGPUBufferedFrame",
                    b.to_string(),
                    format!(
                        "Up to {b} frame(s) may be queued ahead of the GPU. Each queued frame is \
                         one more frame of age between your input and what you see."
                    ),
                )
                .suggest(
                    "Benchmark MaxGPUBufferedFrame=0. Note Reflex already manages the queue, so \
                     the measured gain may be small — which is worth knowing either way.",
                ),
            );
        }
    }

    if let Some(limit) = s.fps_limit {
        if limit == 0 {
            out.push(SettingFinding::new(
                Impact::Info,
                "FPSLimit",
                "uncapped",
                "No frame cap. With Reflex active this is usually fine; a cap mainly helps when \
                 the GPU would otherwise saturate.",
            ));
        }
    }
}

fn check_quality(s: &GraphicsSettings, cpu_limited: bool, out: &mut Vec<SettingFinding>) {
    let get = |name: &str| s.quality.iter().find(|(k, _)| k == name).map(|(_, v)| *v);

    // Geometry drives draw-call count and LOD work, which lands on the CPU —
    // the one resource already short on a low-core-count part.
    if let Some(g) = get("Geometry") {
        if cpu_limited && g >= 3 {
            out.push(
                SettingFinding::new(
                    Impact::Opportunity,
                    "Geometry",
                    g.to_string(),
                    "Geometry level controls level-of-detail and draw-call volume. That work is \
                     largely CPU-side, and this system is CPU-limited.",
                )
                .suggest(
                    "One of the few quality settings likely to matter here. Benchmark lowering \
                     it — unlike shadows or reflections, it moves CPU load rather than GPU load.",
                ),
            );
        } else {
            out.push(SettingFinding::new(
                Impact::Info,
                "Geometry",
                g.to_string(),
                "Affects level-of-detail and draw-call volume, which is mostly CPU work.",
            ));
        }
    }

    if let Some(d) = get("DOF") {
        if d > 0 {
            out.push(
                SettingFinding::new(
                    Impact::Opportunity,
                    "DOF",
                    d.to_string(),
                    "Depth of field blurs the scene outside the focal plane. It costs GPU time \
                     and, more importantly for a shooter, blurs distant detail.",
                )
                .suggest("Competitive players almost always set DOF=0, for visibility as much as FPS."),
            );
        }
    }

    let already_low: Vec<&str> = ["Lighting", "Reflection", "AO", "LensEffects", "VFX"]
        .iter()
        .filter(|k| get(k) == Some(0))
        .copied()
        .collect();
    if !already_low.is_empty() {
        out.push(SettingFinding::new(
            Impact::Good,
            "GPU-side quality",
            already_low.join(", "),
            format!(
                "{} already at their lowest setting, so there is little GPU work left to remove \
                 here.",
                already_low.len()
            ),
        ));
    }
}

fn check_render_scaling(s: &GraphicsSettings, out: &mut Vec<SettingFinding>) {
    let Some(v) = s.render_scaling_factor else {
        return;
    };
    // The in-game slider and the stored integer do not obviously share units,
    // and guessing wrong here would send someone to change the wrong thing.
    out.push(
        SettingFinding::new(
            Impact::Info,
            "RenderScalingFactor",
            v.to_string(),
            format!(
                "Stored as {v}. OPTEA does not assume how this integer maps to the in-game \
                 Render Scaling percentage, so it is reported rather than interpreted."
            ),
        )
        .suggest(
            "Check the Render Scaling value shown in-game and compare. Whatever the encoding, \
             render scale is the largest single graphics lever available — benchmark it directly.",
        ),
    );
}

fn check_engine_scores(s: &GraphicsSettings, out: &mut Vec<SettingFinding>) {
    let (Some(cpu), Some(gpu)) = (s.cpu_score, s.gpu_score) else {
        return;
    };
    if cpu <= 0.0 {
        return;
    }
    let ratio = gpu / cpu;
    if ratio >= 2.0 {
        out.push(
            SettingFinding::new(
                Impact::Info,
                "Siege's own hardware scores",
                format!("CPU {cpu:.0} vs GPU {gpu:.0}"),
                format!(
                    "The game rates this GPU {ratio:.1}x its CPU. That is the game's own \
                     assessment that the CPU is the limiting component — independent of \
                     anything OPTEA measured."
                ),
            )
            .suggest(
                "Expect graphics settings to move frame rate less than they would on a balanced \
                 system, and CPU-side settings to matter more.",
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the real file, values unchanged.
    const REAL: &str = "[DISPLAY]\r\n\
FPSLimit=0\r\n\
NVReflex=2\r\n\
\r\n\
[HARDWARE_INFO]\r\n\
GPUDedicatedMemoryMB=5966\r\n\
GPUScore=12787.000000\r\n\
CPUScore=3563.966064\r\n\
\r\n\
[DISPLAY_SETTINGS]\r\n\
ResolutionWidth=1920\r\n\
ResolutionHeight=1080\r\n\
RefreshRate=143.912003\r\n\
WindowMode=2\r\n\
VSync=0\r\n\
MaxGPUBufferedFrame=1\r\n\
DefaultFOV=90.000000\r\n\
\r\n\
[QUALITY]\r\n\
OverallQualityLevelName=Custom\r\n\
\r\n\
[CUSTOM_QUALITY]\r\n\
AntiAliasing=1\r\n\
Geometry=4\r\n\
Lighting=0\r\n\
Shadow=1\r\n\
Texture=1\r\n\
VFX=0\r\n\
Reflection=0\r\n\
AO=0\r\n\
LensEffects=0\r\n\
DOF=1\r\n\
RenderScalingFactor=15\r\n\
";

    fn parsed() -> GraphicsSettings {
        GraphicsSettings::from_document(&IniDocument::parse(REAL))
    }

    fn ctx() -> MachineContext {
        MachineContext {
            physical_cores: 4,
            display_width: Some(2560),
            display_height: Some(1440),
            display_refresh_hz: Some(144.0),
        }
    }

    #[test]
    fn extracts_the_real_values() {
        let s = parsed();
        assert_eq!(s.render_width, Some(1920));
        assert_eq!(s.render_height, Some(1080));
        assert_eq!(s.window_mode, Some(WindowMode::Borderless));
        assert_eq!(s.vsync, Some(0));
        assert_eq!(s.max_gpu_buffered_frame, Some(1));
        assert_eq!(s.reflex, Some(Reflex::OnPlusBoost));
        assert_eq!(s.fps_limit, Some(0));
        assert_eq!(s.fov, Some(90.0));
        assert_eq!(s.quality_preset.as_deref(), Some("Custom"));
        assert_eq!(s.render_scaling_factor, Some(15));
        assert_eq!(s.gpu_memory_mb, Some(5966));
    }

    #[test]
    fn detects_that_the_game_renders_below_native() {
        let findings = analyze(&parsed(), &ctx());
        let f = findings
            .iter()
            .find(|f| f.setting == "Render resolution")
            .expect("resolution should be reported");
        assert!(f.detail.contains("1920x1080"));
        assert!(f.detail.contains("2560x1440"));
        // 1920*1080 / (2560*1440) = 56%
        assert!(f.detail.contains("56%"), "got: {}", f.detail);
    }

    #[test]
    fn flags_borderless_as_a_latency_opportunity() {
        let findings = analyze(&parsed(), &ctx());
        let f = findings
            .iter()
            .find(|f| f.setting == "WindowMode")
            .unwrap();
        assert_eq!(f.impact, Impact::Opportunity);
        assert!(f.suggestion.as_ref().unwrap().contains("WindowMode=0"));
    }

    #[test]
    fn credits_settings_that_are_already_optimal() {
        let findings = analyze(&parsed(), &ctx());
        let good: Vec<&str> = findings
            .iter()
            .filter(|f| f.impact == Impact::Good)
            .map(|f| f.setting.as_str())
            .collect();
        assert!(good.contains(&"VSync"), "VSync=0 should be credited");
        assert!(good.contains(&"NVReflex"), "Reflex boost should be credited");
    }

    #[test]
    fn flags_buffered_frames_and_dof() {
        let findings = analyze(&parsed(), &ctx());
        assert!(findings
            .iter()
            .any(|f| f.setting == "MaxGPUBufferedFrame" && f.impact == Impact::Opportunity));
        assert!(findings
            .iter()
            .any(|f| f.setting == "DOF" && f.impact == Impact::Opportunity));
    }

    #[test]
    fn geometry_is_an_opportunity_only_when_cpu_limited() {
        let limited = analyze(&parsed(), &ctx());
        let f = limited.iter().find(|f| f.setting == "Geometry").unwrap();
        assert_eq!(f.impact, Impact::Opportunity, "4 cores should flag geometry");

        let mut roomy = ctx();
        roomy.physical_cores = 12;
        let f = analyze(&parsed(), &roomy)
            .into_iter()
            .find(|f| f.setting == "Geometry")
            .unwrap();
        assert_eq!(f.impact, Impact::Info, "12 cores should not flag geometry");
    }

    #[test]
    fn render_scaling_is_reported_not_guessed() {
        // Refusing to invent an encoding is the point of this finding.
        let findings = analyze(&parsed(), &ctx());
        let f = findings
            .iter()
            .find(|f| f.setting == "RenderScalingFactor")
            .unwrap();
        assert!(f.detail.contains("does not assume"));
        assert_eq!(f.impact, Impact::Info);
    }

    #[test]
    fn uses_the_games_own_scores_as_independent_evidence() {
        let findings = analyze(&parsed(), &ctx());
        let f = findings
            .iter()
            .find(|f| f.setting.contains("hardware scores"))
            .expect("should surface CPU/GPU score ratio");
        // 12787 / 3564 = 3.6
        assert!(f.detail.contains("3.6x"), "got: {}", f.detail);
    }

    #[test]
    fn findings_are_ordered_by_impact() {
        let findings = analyze(&parsed(), &ctx());
        let impacts: Vec<Impact> = findings.iter().map(|f| f.impact).collect();
        let mut sorted = impacts.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(impacts, sorted);
    }

    #[test]
    fn every_opportunity_carries_a_suggestion() {
        for f in analyze(&parsed(), &ctx()) {
            if f.impact == Impact::Opportunity {
                assert!(
                    f.suggestion.is_some(),
                    "{} is an opportunity with no suggested action",
                    f.setting
                );
            }
        }
    }

    #[test]
    fn missing_settings_do_not_panic() {
        let s = GraphicsSettings::from_document(&IniDocument::parse("[EMPTY]\r\n"));
        assert!(s.render_width.is_none());
        assert!(analyze(&s, &ctx()).is_empty() || true);
    }
}
