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

pub fn quiet_plan(candidates: &[optea_core::quiet::Candidate], include_ask: bool) {
    use optea_core::quiet::Tier;
    let st = Style::detect();

    println!();
    println!("{}", st.bold("BACKGROUND APPS RUNNING"));
    println!(
        "  {}",
        st.dim("Only apps on OPTEA's allowlist appear here. Anti-cheat, the game, the shell, \
                and system processes are never candidates.")
    );
    println!();

    for c in candidates {
        let (code, tag) = match c.tier {
            Tier::Auto => ("32", "CLOSE"),
            Tier::Ask if include_ask => ("33", " ASK "),
            Tier::Ask => ("2", " SKIP"),
        };
        println!(
            "  {}  {}{}",
            st.paint(code, tag),
            st.bold(&c.label),
            st.dim(&c.instance_note())
        );
        println!("        {}", c.why);
        if let Some(cost) = &c.cost {
            println!("        {}", st.dim(&format!("cost: {cost}")));
        }
    }

    let asks = candidates.iter().filter(|c| c.tier == Tier::Ask).count();
    if asks > 0 && !include_ask {
        println!();
        println!(
            "  {}",
            st.dim(&format!(
                "{asks} app(s) that hold your work are skipped. Add --all to be asked about \
                 them, or --yes to close them without prompting."
            ))
        );
    }
    println!();
}

pub fn quiet_result(results: &[optea_core::quiet::CloseResult]) {
    let st = Style::detect();

    println!();
    println!("{}", st.bold("RESULT"));
    if results.is_empty() {
        println!("  {}", st.dim("nothing was closed"));
        println!();
        return;
    }

    let mut closed = 0;
    for r in results {
        if r.fully_closed() {
            closed += 1;
            println!("  {} {}", st.paint("32", "✓"), r.label);
        } else if r.still_running > 0 {
            println!(
                "  {} {} {}",
                st.paint("33", "•"),
                r.label,
                st.dim(&format!(
                    "{} process(es) declined to close — it may be asking you to save",
                    r.still_running
                ))
            );
        } else if r.protected > 0 {
            println!(
                "  {} {} {}",
                st.dim("-"),
                r.label,
                st.dim("protected, refused")
            );
        }
    }

    println!();
    println!("  {} app(s) closed", closed);
    println!(
        "  {}",
        st.dim("OPTEA does not reopen these. Start them again yourself when you are done.")
    );
    println!();
}

pub fn bench_list(store: &optea_core::bench::BenchStore) {
    use optea_core::bench::{MIN_RUNS, RECOMMENDED_RUNS};
    let st = Style::detect();
    let labels = store.labels();

    println!();
    println!("{}", st.bold("RECORDED BENCHMARKS"));
    kv(&st, "Store", &store.dir().display().to_string());
    println!();

    if labels.is_empty() {
        println!("  {}", st.dim("nothing recorded yet"));
        println!(
            "  {}",
            st.dim("start with: optea bench record --label baseline")
        );
        println!();
        return;
    }

    for (label, count) in &labels {
        let note = if *count < MIN_RUNS {
            st.paint("31", "too few to compare")
        } else if *count < RECOMMENDED_RUNS {
            st.paint("33", "enough to compare, but only a large effect would show")
        } else {
            st.paint("32", "ready")
        };
        println!("  {:<28} {:>2} run(s)  {}", st.bold(label), count, note);
    }

    println!();
    println!(
        "  {}",
        st.dim(&format!(
            "{RECOMMENDED_RUNS} runs per label is the point where small effects become resolvable."
        ))
    );
    println!();
}

pub fn bench_recorded(
    run: &optea_core::bench::Run,
    store: &optea_core::bench::BenchStore,
    capture: &optea_metrics::presentmon::Capture,
) {
    use optea_core::bench::{MIN_RUNS, RECOMMENDED_RUNS};
    let st = Style::detect();
    let s = &run.summary;
    let count = store.runs_for(&run.label).len();

    println!();
    println!(
        "{} {}",
        st.bold("RECORDED"),
        st.dim(&format!("as '{}' ({} frames)", run.label, s.frames))
    );
    kv(&st, "Average FPS", &format!("{:.1}", s.avg_fps));
    kv(&st, "1% low FPS", &st.bold(&format!("{:.1}", s.low_1_fps)));
    kv(&st, "Frametime p99", &format!("{:.2} ms", s.frame_time_p99_ms));
    kv(
        &st,
        "Game focused",
        &format!("{:.0}%", run.focused_fraction * 100.0),
    );
    kv(&st, "Capture window", &run.capture.label());
    focus_warning(&st, capture);

    // A label whose runs used different windows is already compromised; say so
    // now rather than at compare time, while it is still cheap to re-record.
    let mut windows: Vec<String> = store
        .runs_for(&run.label)
        .iter()
        .map(|r| r.capture.label())
        .collect();
    windows.sort();
    windows.dedup();
    if windows.len() > 1 {
        println!();
        println!(
            "  {}",
            st.paint("31", "⚠ THIS LABEL NOW MIXES CAPTURE WINDOWS")
        );
        for w in &windows {
            println!("      {}", st.dim(w));
        }
        println!(
            "  {}",
            st.paint(
                "33",
                "Each window samples a different part of the scene, so these runs are not \
                 comparable. Re-record them with identical --delay and --seconds."
            )
        );
    }
    println!();

    if count < MIN_RUNS {
        println!(
            "  {} {} run under '{}'. At least {MIN_RUNS} are needed before any comparison.",
            st.paint("33", "→"),
            count,
            run.label
        );
    } else if count < RECOMMENDED_RUNS {
        println!(
            "  {} {} runs under '{}'. Comparable now, but {RECOMMENDED_RUNS} is where small \
             effects become resolvable.",
            st.paint("33", "→"),
            count,
            run.label
        );
    } else {
        println!(
            "  {} {} runs under '{}' — ready to compare.",
            st.paint("32", "→"),
            count,
            run.label
        );
    }
    println!();
}

pub fn bench_comparison(cmp: &optea_core::bench::Comparison) {
    use optea_metrics::stats::Verdict;
    let st = Style::detect();

    println!();
    println!(
        "{} {} {} {}",
        st.bold("COMPARISON"),
        st.dim(&cmp.baseline_label),
        st.dim("→"),
        st.bold(&cmp.variant_label)
    );
    kv(
        &st,
        "Runs",
        &format!(
            "{} baseline vs {} variant, {:.0}% confidence",
            cmp.baseline_runs,
            cmp.variant_runs,
            cmp.confidence * 100.0
        ),
    );
    println!();

    for e in &cmp.effects {
        let (code, tag) = match e.verdict {
            Verdict::Improvement => ("32", "BETTER"),
            Verdict::Regression => ("31", " WORSE"),
            Verdict::NoDetectableEffect => ("2", "  NONE"),
        };
        println!(
            "  {}  {:<22} {:>8.2} → {:>8.2}   {:+6.1}%   CI [{:+.2}, {:+.2}]",
            st.paint(code, tag),
            e.metric.label(),
            e.baseline_median,
            e.variant_median,
            e.delta_pct,
            e.ci_low,
            e.ci_high
        );
    }

    println!();
    println!("  {}", st.bold(&cmp.conclusion()));

    if !cmp.mixed_windows.is_empty() {
        println!();
        println!(
            "  {}",
            st.paint("31", "⚠ Runs used different capture windows:")
        );
        for w in &cmp.mixed_windows {
            println!("      {}", st.dim(w));
        }
    }
    if !cmp.untrustworthy.is_empty() {
        println!();
        println!(
            "  {}",
            st.paint("31", "⚠ Runs captured while the game was not focused:")
        );
        for r in &cmp.untrustworthy {
            println!("      {}", st.dim(r));
        }
    }
    if cmp.underpowered {
        println!();
        println!(
            "  {}",
            st.paint(
                "33",
                "Few runs per side: only a large effect could be resolved here. A 'NONE' verdict \
                 means 'not proven', not 'proven absent'.",
            )
        );
    }
    if cmp.all_inconclusive() {
        println!(
            "  {}",
            st.dim("This is the expected outcome for most tweaks, and is a real result.")
        );
    }
    println!();
}

pub fn siege_benchmark(r: &optea_game::benchmark::BenchmarkReport) {
    let st = Style::detect();
    println!();
    println!("{}", st.bold("IN-GAME BENCHMARK"));
    kv(&st, "Report", &r.stamp);
    kv(
        &st,
        "Frames",
        &format!("{} over {:.1}s", r.frame_count, r.duration_s),
    );
    if let Some(load) = r.loading_time_ms {
        kv(&st, "Loading time", &format!("{:.1}s", load / 1000.0));
    }
    println!();
    kv(&st, "Average FPS", &format!("{:.1}", r.avg_fps));
    kv(&st, "Highest FPS", &format!("{:.1}", r.highest_fps));
    kv(&st, "Lowest FPS", &st.bold(&format!("{:.1}", r.lowest_fps)));
    kv(
        &st,
        "Worst frame",
        &format!("{:.1} ms", r.largest_frame_time_ms),
    );

    if r.cpu_times_ms.is_empty() {
        println!();
        println!("  {}", st.dim("no CPU/GPU series in the HTML report"));
        println!();
        return;
    }

    println!();
    println!("{}", st.bold("CPU vs GPU TIME  (the engine's own instrumentation)"));
    let row = |label: &str, q: f64| {
        let c = r.cpu_percentile(q).unwrap_or(f64::NAN);
        let g = r.gpu_percentile(q).unwrap_or(f64::NAN);
        // Whichever side is longer is the one gating that frame.
        let marker = if c > g {
            st.paint("33", "CPU")
        } else {
            st.paint("36", "GPU")
        };
        println!(
            "  {:<8} CPU {:>7.2} ms    GPU {:>7.2} ms    {} is the longer pole",
            label, c, g, marker
        );
    };
    row("median", 0.50);
    row("p95", 0.95);
    row("p99", 0.99);

    if let Some(frac) = r.cpu_bound_fraction_after(25) {
        println!();
        kv(
            &st,
            "CPU > GPU",
            &format!("{:.0}% of samples (excluding loading)", frac * 100.0),
        );
    }

    println!();
    match r.tail_is_cpu_bound() {
        Some(true) => {
            println!(
                "  {}",
                st.paint("33", "The slow tail is CPU-bound.")
            );
            println!(
                "  {}",
                st.dim(
                    "Lowering GPU-side quality (shadows, reflections, textures, resolution) \
                     will not move the 1% lows much, because the GPU is not what those frames \
                     are waiting on. CPU-side work — draw calls, geometry, and background \
                     processes stealing cores — is where the stutters come from."
                )
            );
        }
        Some(false) => {
            println!("  {}", st.paint("36", "The slow tail is GPU-bound."));
            println!(
                "  {}",
                st.dim("Resolution and GPU-side quality settings are the effective levers here.")
            );
        }
        None => {}
    }
    println!();
}

pub fn siege_editable() {
    let st = Style::detect();
    println!();
    println!("{}", st.bold("EDITABLE SETTINGS"));
    println!(
        "  {}",
        st.dim("An allowlist. Values outside these ranges are refused rather than written.")
    );
    println!();
    for s in optea_game::settings::EDITABLE {
        println!(
            "  {:<16} {}",
            st.bold(s.alias),
            st.dim(&format!("[{}] {}", s.section, s.key))
        );
        println!("    {}", s.description);
        println!("    {}", st.dim(&format!("allowed: {}", s.allowed.describe())));
        println!();
    }
    println!(
        "  {}",
        st.dim("usage: optea siege set <name> <value>   (add --dry-run to preview)")
    );
    println!();
}

pub fn siege_set(
    setting: &optea_game::settings::EditableSetting,
    before: &str,
    after: i64,
    report: &optea_game::backup::EditReport,
) {
    let st = Style::detect();
    println!();
    if !report.changed {
        println!(
            "  {} [{}] {} is already {after} — nothing written",
            st.dim("-"),
            setting.section,
            setting.key
        );
        println!();
        return;
    }

    println!(
        "  {} [{}] {}  {} {}",
        st.paint("32", "✓"),
        setting.section,
        st.bold(setting.key),
        st.dim(&format!("{before} →")),
        st.bold(&after.to_string())
    );
    println!();
    kv(
        &st,
        "Backup",
        &format!("{} ({})", report.backup_id, report.backup_path.display()),
    );
    kv(&st, "Pristine", &report.pristine_path.display().to_string());
    kv(
        &st,
        "Size",
        &format!("{} → {} bytes", report.bytes_before, report.bytes_after),
    );
    println!();
    println!(
        "  {}",
        st.dim("undo this file entirely with: optea siege restore pristine")
    );
    println!();
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

/// Warn prominently when a capture was taken with the game in the background.
fn focus_warning(st: &Style, capture: &optea_metrics::presentmon::Capture) {
    if capture.is_trustworthy() {
        return;
    }
    println!();
    println!(
        "  {}",
        st.paint("31", "⚠ THESE NUMBERS ARE NOT USABLE")
    );
    println!("  {}", st.paint("33", &capture.focus_note()));
    println!(
        "  {}",
        st.dim("Focus the game window and capture again.")
    );
}

pub fn summary(s: &optea_metrics::Summary, capture: &optea_metrics::presentmon::Capture) {
    let st = Style::detect();
    println!();
    println!("{}", st.bold("CAPTURE"));
    kv(
        &st,
        "Frames",
        &format!("{} over {:.1}s", s.frames, s.duration_s),
    );
    kv(
        &st,
        "Game focused",
        &format!("{:.0}%", capture.focus.focused_fraction() * 100.0),
    );
    focus_warning(&st, capture);
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
