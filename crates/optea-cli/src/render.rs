//! Terminal rendering.

use optea_core::doctor::{Report, Severity};
use optea_core::engine::{ApplyResult, Outcome};
use optea_core::tweak::{Applicability, Risk, SystemInfo, Tweak};
use optea_sys::registry::RegValue;
use std::path::Path;

// ANSI colours, suppressed when NO_COLOR is set or output is redirected.
struct Style {
    enabled: bool,
}

impl Style {
    fn detect() -> Self {
        Style {
            enabled: std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }

    fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }

    fn severity(&self, sev: Severity) -> String {
        let code = match sev {
            Severity::Good => "32",     // green
            Severity::Info => "36",     // cyan
            Severity::Warn => "33",     // yellow
            Severity::Critical => "31", // red
        };
        self.paint(code, &format!("{:>4}", sev.label()))
    }
}

pub fn report(r: &Report) {
    let st = Style::detect();

    println!();
    println!("{}", st.bold("SYSTEM"));
    kv(&st, "OS", &r.os.version_string());
    kv(
        &st,
        "CPU",
        &format!(
            "{} — {} cores / {} threads",
            r.cpu.name, r.cpu.physical_cores, r.cpu.logical_processors
        ),
    );
    if let Some(p) = &r.power {
        let parking = match p.min_cores_percent_ac {
            Some(100) => "core parking off".to_string(),
            Some(pct) => format!("core parking floor {pct}%"),
            None => "core parking unknown".to_string(),
        };
        kv(&st, "Power", &format!("{} — {}", p.scheme_name, parking));
    }
    kv(
        &st,
        "Elevated",
        if r.elevated { "yes" } else { "no" },
    );

    println!();
    println!("{}", st.bold("DISPLAYS"));
    for d in &r.displays {
        let connector = d
            .connector
            .map(|c| c.as_str())
            .unwrap_or_else(|| "?".into());
        let monitor = d.monitor_name.as_deref().unwrap_or("Unknown monitor");
        let primary = if d.is_primary { " [primary]" } else { "" };
        kv(
            &st,
            &d.gdi_name,
            &format!(
                "{} — {} over {} on {}{}",
                monitor,
                d.mode_string(),
                connector,
                d.gpu_name,
                primary
            ),
        );
    }

    println!();
    println!("{}", st.bold("DISPLAY ADAPTERS"));
    for g in &r.gpus {
        let status = match g.problem_text() {
            Some(p) => p.to_string(),
            None => "healthy".into(),
        };
        kv(&st, &g.name, &status);
    }

    println!();
    println!("{}", st.bold("REGISTRY STATE"));
    for probe in &r.registry {
        kv(&st, probe.label, &format_value(probe.value.as_ref()));
    }

    if let Some(s) = &r.siege {
        println!();
        println!("{}", st.bold("RAINBOW SIX SIEGE"));
        kv(&st, "Settings root", &s.root);
        kv(&st, "Profiles", &s.profile_count.to_string());
        if let Some(active) = &s.active_profile {
            kv(&st, "Active profile", active);
        }
    }

    println!();
    println!("{}", st.bold("FINDINGS"));
    if r.findings.is_empty() {
        println!("  nothing to report");
    }
    for f in &r.findings {
        println!("  {}  {}", st.severity(f.severity), st.bold(&f.title));
        println!("        {}", f.detail);
        if let Some(a) = &f.advice {
            println!("        {}", st.dim(&format!("→ {a}")));
        }
    }

    println!();
    let counts = |sev: Severity| r.findings.iter().filter(|f| f.severity == sev).count();
    println!(
        "{}",
        st.dim(&format!(
            "{} critical, {} warnings, {} info, {} already good",
            counts(Severity::Critical),
            counts(Severity::Warn),
            counts(Severity::Info),
            counts(Severity::Good),
        ))
    );
    println!();
}

fn kv(st: &Style, key: &str, value: &str) {
    println!("  {:<22} {}", st.dim(key), value);
}

pub fn siege_settings(
    s: &optea_game::settings::GraphicsSettings,
    findings: &[optea_game::settings::SettingFinding],
    ctx: &optea_game::settings::MachineContext,
) {
    use optea_game::settings::Impact;
    let st = Style::detect();

    println!();
    println!("{}", st.bold("GAME SETTINGS"));
    kv(&st, "Render resolution", &s.resolution_label());
    if let (Some(w), Some(h)) = (ctx.display_width, ctx.display_height) {
        kv(
            &st,
            "Display",
            &format!(
                "{w}x{h}{}",
                ctx.display_refresh_hz
                    .map(|r| format!(" @ {r:.0} Hz"))
                    .unwrap_or_default()
            ),
        );
    }
    if let Some(m) = s.window_mode {
        kv(&st, "Window mode", &m.label());
    }
    if let Some(r) = s.reflex {
        kv(&st, "NVIDIA Reflex", &r.label());
    }
    if let Some(v) = s.vsync {
        kv(&st, "VSync", if v == 0 { "off" } else { "on" });
    }
    if let Some(b) = s.max_gpu_buffered_frame {
        kv(&st, "Buffered frames", &b.to_string());
    }
    if let Some(f) = s.fps_limit {
        kv(
            &st,
            "FPS limit",
            &if f == 0 { "uncapped".into() } else { f.to_string() },
        );
    }
    if let Some(f) = s.fov {
        kv(&st, "FOV", &format!("{f:.0}"));
    }
    if let Some(p) = &s.quality_preset {
        kv(&st, "Quality preset", p);
    }

    if !s.quality.is_empty() {
        println!();
        println!("{}", st.bold("QUALITY"));
        let pairs: Vec<String> = s
            .quality
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        for chunk in pairs.chunks(4) {
            println!("  {}", st.dim(&chunk.join("   ")));
        }
    }

    println!();
    println!("{}", st.bold("ANALYSIS"));
    for f in findings {
        let (code, tag) = match f.impact {
            Impact::Good => ("32", "  OK"),
            Impact::Info => ("36", "INFO"),
            Impact::Opportunity => ("33", "TEST"),
        };
        println!(
            "  {}  {} {}",
            st.paint(code, tag),
            st.bold(&f.setting),
            st.dim(&format!("= {}", f.current))
        );
        println!("        {}", f.detail);
        if let Some(sug) = &f.suggestion {
            println!("        {}", st.dim(&format!("→ {sug}")));
        }
    }

    let count = |i: Impact| findings.iter().filter(|f| f.impact == i).count();
    println!();
    println!(
        "{}",
        st.dim(&format!(
            "{} worth benchmarking, {} informational, {} already good",
            count(Impact::Opportunity),
            count(Impact::Info),
            count(Impact::Good),
        ))
    );
    println!(
        "{}",
        st.dim("Nothing here is applied automatically, and no gain is claimed until measured.")
    );
    println!();
}

pub fn siege_status(
    profile: &optea_game::profile::SiegeProfile,
    file: &optea_game::GuardedFile,
) {
    let st = Style::detect();
    println!();
    println!("{}", st.bold("SIEGE SETTINGS"));
    kv(&st, "Profile", &profile.id);
    kv(&st, "File", &file.target().display().to_string());

    match std::fs::read(file.target()) {
        Ok(bytes) => {
            kv(&st, "Size", &format!("{} bytes", bytes.len()));
            kv(
                &st,
                "SHA-256",
                &optea_game::backup::sha256_hex(&bytes)[..32].to_string(),
            );
            let crlf = bytes.windows(2).filter(|w| w == b"\r\n").count();
            kv(&st, "Line endings", &format!("{crlf} CRLF"));
        }
        Err(e) => kv(&st, "File", &format!("unreadable: {e}")),
    }

    println!();
    println!("{}", st.bold("BACKUPS"));
    kv(&st, "Store", &file.store().dir().display().to_string());
    match file.store().pristine() {
        Some(p) => {
            let ok = p.verify().is_ok();
            kv(
                &st,
                "Pristine",
                &format!(
                    "{} ({} bytes) {}",
                    p.meta.taken_at,
                    p.meta.size,
                    if ok {
                        st.paint("32", "verified")
                    } else {
                        st.paint("31", "FAILED VERIFICATION")
                    }
                ),
            );
        }
        None => kv(
            &st,
            "Pristine",
            &st.dim("none yet — taken automatically on first edit"),
        ),
    }
    let history = file.store().history();
    kv(&st, "Rolling backups", &history.len().to_string());
    for b in history.iter().take(5) {
        let ok = if b.verify().is_ok() { "ok" } else { "CORRUPT" };
        println!("    {}  {} bytes  {}", st.dim(&b.meta.id), b.meta.size, ok);
    }

    println!();
    println!("{}", st.bold("EDITABLE RIGHT NOW?"));
    match file.preflight() {
        Ok(()) => println!("  {} safe to edit", st.paint("32", "yes —")),
        Err(e) => {
            println!("  {} {}", st.paint("33", "no —"), e);
            println!(
                "  {}",
                st.dim("taking a backup is still safe; it only reads the file")
            );
        }
    }
    println!();
}

pub fn measure_check(d: &optea_metrics::presentmon::Diagnostics) {
    let st = Style::detect();
    let (maj, min, patch) = d.api_version;

    println!();
    println!("{}", st.bold("PRESENTMON CHECK"));
    kv(&st, "DLL", &d.dll_path);
    kv(&st, "API version", &format!("{maj}.{min}.{patch}"));
    kv(
        &st,
        "Table verified vs",
        &format!("{}.{}", d.expected_api.0, d.expected_api.1),
    );
    kv(
        &st,
        "Session",
        &if d.session_opened {
            st.paint("32", "opened")
        } else {
            st.paint("31", "failed")
        },
    );

    match d.blob_size {
        Some(size) => {
            kv(&st, "Frame blob", &format!("{size} bytes"));
            println!();
            println!("  {}", st.dim("metric offsets within each frame blob:"));
            for (symbol, offset) in &d.offsets {
                println!("    {:<40} +{offset}", st.dim(symbol));
            }
            println!();
            println!(
                "  {}",
                st.paint("32", "Frame query registered — the FFI works against this service.")
            );
        }
        None => {
            println!();
            println!(
                "  {}",
                st.paint("31", "Frame query registration FAILED — the metric table may be stale.")
            );
        }
    }
    println!();
}

pub fn summary(s: &optea_metrics::Summary) {
    let st = Style::detect();
    println!();
    println!("{}", st.bold("CAPTURE"));
    kv(
        &st,
        "Frames",
        &format!("{} over {:.1}s", s.frames, s.duration_s),
    );
    println!();
    kv(&st, "Average FPS", &format!("{:.1}", s.avg_fps));
    // The lows are what a competitive player actually feels.
    kv(
        &st,
        "1% low FPS",
        &st.bold(&format!("{:.1}", s.low_1_fps)),
    );
    kv(&st, "0.1% low FPS", &format!("{:.1}", s.low_01_fps));
    println!();
    kv(
        &st,
        "Frametime p50",
        &format!("{:.2} ms", s.frame_time_p50_ms),
    );
    kv(
        &st,
        "Frametime p99",
        &format!("{:.2} ms", s.frame_time_p99_ms),
    );
    match s.gpu_busy_mean_ms {
        Some(v) => kv(&st, "GPU busy (mean)", &format!("{v:.2} ms", )),
        None => kv(&st, "GPU busy (mean)", &st.dim("not reported")),
    }
    match s.input_latency_p50_ms {
        Some(v) => kv(&st, "Input latency p50", &format!("{v:.2} ms")),
        None => kv(
            &st,
            "Input latency p50",
            &st.dim("not reported (needs a title that emits input markers)"),
        ),
    }

    // A CPU-bound frame is one the GPU finished early and then waited on.
    if let Some(gpu) = s.gpu_busy_mean_ms {
        let cpu_bound = gpu < s.frame_time_p50_ms * 0.9;
        println!();
        if cpu_bound {
            println!(
                "  {}",
                st.paint(
                    "33",
                    &format!(
                        "GPU busy {:.2} ms vs {:.2} ms frametime — GPU is idle most of the frame, \
                         so this is CPU-bound.",
                        gpu, s.frame_time_p50_ms
                    )
                )
            );
        } else {
            println!(
                "  {}",
                st.dim(&format!(
                    "GPU busy {:.2} ms of {:.2} ms frametime — GPU-bound.",
                    gpu, s.frame_time_p50_ms
                ))
            );
        }
    }
    println!();
}

fn risk_tag(st: &Style, risk: Risk) -> String {
    let code = match risk {
        Risk::Safe => "32",
        Risk::Moderate => "33",
        Risk::Deep => "31",
    };
    st.paint(code, &format!("{:<8}", risk.label()))
}

/// State of a catalog entry on this system, as a short label.
fn state_label(st: &Style, t: &dyn Tweak, sys: &SystemInfo) -> String {
    match t.applicable(sys) {
        Applicability::Applicable => st.paint("36", "can apply"),
        Applicability::AlreadySet => st.paint("32", "already set"),
        Applicability::NotApplicable { .. } => st.dim("n/a"),
    }
}

pub fn catalog(entries: &[Box<dyn Tweak>], sys: &SystemInfo) {
    let st = Style::detect();

    println!();
    println!("{}", st.bold("TWEAK CATALOG"));
    println!(
        "  {}",
        st.dim("Every entry is unverified until benchmarked on this machine.")
    );
    println!();

    for t in entries {
        println!(
            "  {} {}  {}",
            risk_tag(&st, t.risk()),
            st.bold(t.id()),
            state_label(&st, t.as_ref(), sys)
        );
        println!("    {}", t.title());
        println!("    {}", st.dim(t.description()));

        if let Applicability::NotApplicable { reason } = t.applicable(sys) {
            println!("    {}", st.dim(&format!("skipped: {reason}")));
        }
        let mut notes = vec![format!("evidence: {}", t.evidence().label())];
        if t.requires_reboot() {
            notes.push("needs reboot".into());
        }
        if let Ok(current) = t.probe() {
            notes.push(format!("current: {current}"));
        }
        println!("    {}", st.dim(&notes.join("  |  ")));
        println!();
    }
}

/// JSON shape for `optea list --json`.
pub fn catalog_json(entries: &[Box<dyn Tweak>], sys: &SystemInfo) -> serde_json::Value {
    let items: Vec<serde_json::Value> = entries
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id(),
                "title": t.title(),
                "description": t.description(),
                "risk": t.risk().label(),
                "evidence": t.evidence().label(),
                "requires_reboot": t.requires_reboot(),
                "applicability": t.applicable(sys),
                "current": t.probe().unwrap_or_else(|e| format!("error: {e}")),
            })
        })
        .collect();
    serde_json::json!({ "tweaks": items })
}

pub fn dry_run(entries: &[Box<dyn Tweak>], sys: &SystemInfo) {
    let st = Style::detect();
    println!();
    println!("{} {}", st.bold("DRY RUN"), st.dim("— nothing was changed"));
    println!();

    let mut would_apply = 0;
    for t in entries {
        match t.applicable(sys) {
            Applicability::Applicable => {
                would_apply += 1;
                println!(
                    "  {} {}  {}",
                    risk_tag(&st, t.risk()),
                    st.bold(t.id()),
                    st.paint("36", "would apply")
                );
                if let Ok(current) = t.probe() {
                    println!("    {}", st.dim(&format!("current: {current}")));
                }
            }
            Applicability::AlreadySet => println!(
                "  {} {}  {}",
                risk_tag(&st, t.risk()),
                st.bold(t.id()),
                st.paint("32", "already set — skip")
            ),
            Applicability::NotApplicable { reason } => println!(
                "  {} {}  {}",
                risk_tag(&st, t.risk()),
                st.bold(t.id()),
                st.dim(&format!("skip: {reason}"))
            ),
        }
    }

    println!();
    println!(
        "{}",
        st.dim(&format!(
            "{would_apply} of {} entries would change. Re-run without --dry-run to apply.",
            entries.len()
        ))
    );
    println!();
}

pub fn apply_result(result: &ApplyResult, snapshot_dir: &Path) {
    let st = Style::detect();
    println!();
    println!("{}", st.bold("APPLIED"));

    let mut applied = 0;
    for (id, outcome) in &result.outcomes {
        match outcome {
            Outcome::Applied => {
                applied += 1;
                println!("  {} {id}", st.paint("32", "✓"));
            }
            Outcome::AlreadySet => println!("  {} {id} {}", st.dim("-"), st.dim("already set")),
            Outcome::Skipped { reason } => {
                println!("  {} {id} {}", st.dim("-"), st.dim(reason))
            }
        }
    }

    println!();
    println!("  {} tweak(s) applied", applied);
    println!(
        "  {}",
        st.dim(&format!(
            "snapshot {} in {}",
            result.transaction_id,
            snapshot_dir.display()
        ))
    );
    println!(
        "  {}",
        st.dim(&format!("undo with: optea revert {}", result.transaction_id))
    );
    if result.requires_reboot {
        println!();
        println!(
            "  {}",
            st.paint("33", "Some changes take effect only after a restart.")
        );
    }
    println!();
}

fn format_value(v: Option<&RegValue>) -> String {
    match v {
        None => "not set".into(),
        Some(RegValue::Dword(d)) => format!("{d} (0x{d:x})"),
        Some(RegValue::Qword(q)) => format!("{q}"),
        Some(RegValue::Sz(s)) | Some(RegValue::ExpandSz(s)) => s.clone(),
        Some(RegValue::MultiSz(items)) => items.join(", "),
        Some(RegValue::Binary(b)) => format!("{} bytes", b.len()),
        Some(RegValue::None) => "REG_NONE".into(),
    }
}
