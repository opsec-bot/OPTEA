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
