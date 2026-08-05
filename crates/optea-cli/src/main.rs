//! OPTEA command line.

mod render;

use anyhow::{bail, Result};
use optea_core::tweak::{Risk, SystemInfo, Tweak};

const USAGE: &str = "\
optea — Rainbow Six Siege system tuning, measured rather than assumed

USAGE:
    optea <COMMAND> [OPTIONS]

COMMANDS:
    doctor              Read-only system report. Changes nothing.
    measure check       Verify the PresentMon service and frame query work.
    measure capture     Capture frames from a running game and summarise them.
    siege status        Show the Siege profile, settings file, and backup state.
    siege settings      Read GameSettings.ini and analyse it for this hardware.
    siege set <k> <v>   Change a setting. Backs up and verifies first.
    siege editable      List the settings OPTEA is willing to write.
    siege benchmark     Read the game's own benchmark report (CPU vs GPU time).
    siege backup        Take a verified backup now. Safe while the game runs.
    siege restore       Restore the settings file from a backup.
    bench record        Capture a run and store it under a label.
    bench list          Show recorded labels and run counts.
    bench compare       A/B two labels and report whether the difference is real.
    list                Show the tweak catalog and each entry's current state.
    apply               Apply tweaks. Captures a revertible snapshot first.
    revert [<id>]       Undo a transaction. Defaults to the most recent.
    history             List recorded transactions.
    help                Show this message.

OPTIONS:
    --json              Machine-readable output (doctor, list, history).
    --risk <level>      Highest risk to apply: safe | moderate | deep.
                        Default: safe. 'deep' additionally requires --i-understand.
    --only <ids>        Comma-separated tweak ids, instead of a whole risk tier.
    --dry-run           Show what apply would do, without changing anything.
    --i-understand      Required opt-in for deep-risk tweaks. These can leave a
                        machine unbootable; a restore point is strongly advised.
    --pid <n>           Process to capture. Defaults to a detected game.
    --seconds <n>       Capture duration. Default 10.
    --label <name>      Label to record a benchmark run under.
    --note <text>       Note stored with a run (map, scene, settings).
    --confidence <p>    Confidence level for comparisons. Default 0.95.
    --wait <n>          Wait up to n seconds for the game to gain focus before
                        capturing. Use this so you can alt-tab into the game
                        and start its benchmark before the capture begins.
    --delay <n>         After focus is gained, skip n seconds before capturing.
                        Lets you start the benchmark so menu frames are excluded.

Applying or reverting requires an elevated (administrator) terminal.
";

struct Args {
    command: String,
    json: bool,
    dry_run: bool,
    understand: bool,
    risk: Risk,
    only: Vec<String>,
    pid: Option<u32>,
    seconds: u64,
    label: Option<String>,
    note: String,
    confidence: f64,
    wait: u64,
    delay: u64,
    positional: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut args = Args {
        command: String::new(),
        json: false,
        dry_run: false,
        understand: false,
        risk: Risk::Safe,
        only: Vec::new(),
        pid: None,
        seconds: 10,
        label: None,
        note: String::new(),
        confidence: 0.95,
        wait: 0,
        delay: 0,
        positional: Vec::new(),
    };

    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--json" => args.json = true,
            "--dry-run" => args.dry_run = true,
            "--i-understand" => args.understand = true,
            "--risk" => {
                i += 1;
                let v = raw.get(i).map(String::as_str).unwrap_or("");
                args.risk = match v {
                    "safe" => Risk::Safe,
                    "moderate" => Risk::Moderate,
                    "deep" => Risk::Deep,
                    other => bail!("unknown risk level '{other}' (safe | moderate | deep)"),
                };
            }
            "--pid" => {
                i += 1;
                let v = raw.get(i).map(String::as_str).unwrap_or("");
                args.pid = Some(v.parse().map_err(|_| anyhow::anyhow!("bad --pid '{v}'"))?);
            }
            "--seconds" => {
                i += 1;
                let v = raw.get(i).map(String::as_str).unwrap_or("");
                args.seconds = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("bad --seconds '{v}'"))?;
            }
            "--label" => {
                i += 1;
                args.label = raw.get(i).cloned();
            }
            "--wait" => {
                i += 1;
                let v = raw.get(i).map(String::as_str).unwrap_or("");
                args.wait = v.parse().map_err(|_| anyhow::anyhow!("bad --wait '{v}'"))?;
            }
            "--delay" => {
                i += 1;
                let v = raw.get(i).map(String::as_str).unwrap_or("");
                args.delay = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("bad --delay '{v}'"))?;
            }
            "--note" => {
                i += 1;
                args.note = raw.get(i).cloned().unwrap_or_default();
            }
            "--confidence" => {
                i += 1;
                let v = raw.get(i).map(String::as_str).unwrap_or("");
                args.confidence = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("bad --confidence '{v}'"))?;
                if !(0.5..1.0).contains(&args.confidence) {
                    bail!("--confidence must be between 0.5 and 1.0");
                }
            }
            "--only" => {
                i += 1;
                let v = raw.get(i).map(String::as_str).unwrap_or("");
                args.only = v
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            other if other.starts_with("--") => bail!("unknown option '{other}'"),
            other => {
                if args.command.is_empty() {
                    args.command = other.to_string();
                } else {
                    args.positional.push(other.to_string());
                }
            }
        }
        i += 1;
    }

    if args.command.is_empty() {
        args.command = "help".into();
    }
    Ok(args)
}

fn main() -> Result<()> {
    let args = parse_args()?;

    match args.command.as_str() {
        "doctor" => {
            let report = optea_core::doctor::run()?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render::report(&report);
            }
        }
        "measure" => cmd_measure(&args)?,
        "siege" => cmd_siege(&args)?,
        "bench" => cmd_bench(&args)?,
        "list" => cmd_list(&args)?,
        "apply" => cmd_apply(&args)?,
        "revert" => cmd_revert(&args)?,
        "history" => cmd_history(&args)?,
        "help" | "-h" | "--help" => print!("{USAGE}"),
        other => {
            eprint!("unknown command '{other}'\n\n{USAGE}");
            bail!("unknown command");
        }
    }
    Ok(())
}

fn cmd_measure(args: &Args) -> Result<()> {
    match args.positional.first().map(String::as_str) {
        Some("check") | None => {
            let d = optea_metrics::presentmon::diagnose()?;
            render::measure_check(&d);
            Ok(())
        }
        Some("capture") => cmd_capture(args),
        Some(other) => bail!("unknown measure subcommand '{other}' (try: check | capture)"),
    }
}

/// Resolve the target pid, optionally waiting for it to come to the foreground,
/// then capture.
fn capture_with_focus(
    args: &Args,
    pid: u32,
) -> Result<optea_metrics::presentmon::Capture> {
    use std::time::Duration;

    if args.wait > 0 {
        println!(
            "waiting up to {}s for pid {pid} to come to the foreground — \
             alt-tab into the game now",
            args.wait
        );
        let focused = optea_sys::foreground::wait_for_focus(
            pid,
            Duration::from_secs(args.wait),
            |left| {
                if left % 5 == 0 && left > 0 {
                    println!("  {left}s...");
                }
            },
        );
        if !focused {
            bail!(
                "the game never came to the foreground within {}s — nothing was captured, \
                 since a background capture would only measure the engine's idle throttle",
                args.wait
            );
        }
        println!("game has focus");
    } else {
        println!("capturing pid {pid} for {}s...", args.seconds);
    }

    if args.delay > 0 {
        // Excludes menu navigation and the benchmark's own warm-up, which are
        // not the workload being measured.
        println!("  skipping {}s before capture starts...", args.delay);
        for left in (1..=args.delay).rev() {
            if left % 5 == 0 || left <= 3 {
                println!("    {left}s...");
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        // Losing focus during the lead-in means the run is already invalid.
        if !optea_sys::foreground::is_foreground(pid) {
            bail!("the game lost focus during the lead-in — nothing was captured");
        }
    }

    if args.wait > 0 || args.delay > 0 {
        println!("capturing for {}s", args.seconds);
    }

    let mut session = optea_metrics::presentmon::Session::open()?;
    session.track(pid)?;
    Ok(session.capture(
        pid,
        Duration::from_secs(args.seconds),
        Duration::from_millis(200),
    )?)
}

/// Fixed so a verdict is reproducible across invocations. Change it only to
/// check deliberately that a result is not an artefact of one resampling.
const BOOTSTRAP_SEED: u64 = 0x0071_EA00;

fn cmd_bench(args: &Args) -> Result<()> {
    
    let store = optea_core::bench::BenchStore::with_default_dir()?;

    match args.positional.first().map(String::as_str) {
        Some("list") | None => {
            render::bench_list(&store);
            Ok(())
        }
        Some("record") => {
            let label = args
                .label
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--label is required (e.g. --label baseline)"))?;
            let pid = match args.pid {
                Some(p) => p,
                None => detect_game_pid()
                    .ok_or_else(|| anyhow::anyhow!("no known game running — pass --pid <n>"))?,
            };

            let capture = capture_with_focus(args, pid)?;

            // Record which tweaks are active, so a run's provenance is stored
            // with it rather than relying on memory.
            let sys = SystemInfo::query()?;
            let active: Vec<String> = optea_core::catalog::all(&sys)
                .iter()
                .filter(|t| t.applicable(&sys) == optea_core::Applicability::AlreadySet)
                .map(|t| t.id().to_string())
                .collect();

            let run = store.record(
                &label,
                &capture.frames,
                active,
                &args.note,
                capture.focus.focused_fraction(),
            )?;
            render::bench_recorded(&run, &store, &capture);
            Ok(())
        }
        Some("compare") => {
            let baseline = args
                .positional
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: optea bench compare <baseline> <variant>"))?;
            let variant = args
                .positional
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: optea bench compare <baseline> <variant>"))?;

            // Seed is fixed so a verdict is reproducible; vary it deliberately
            // only to check a result is not an artefact of one resampling.
            let cmp = store.compare(baseline, variant, args.confidence, BOOTSTRAP_SEED)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&cmp)?);
            } else {
                render::bench_comparison(&cmp);
            }
            Ok(())
        }
        Some(other) => bail!("unknown bench subcommand '{other}' (record | list | compare)"),
    }
}

/// Resolve the active Siege profile and a guarded handle to its settings file.
fn siege_file() -> Result<(optea_game::profile::SiegeProfile, optea_game::GuardedFile)> {
    let profiles = optea_game::profile::discover()?
        .ok_or_else(|| anyhow::anyhow!("no Siege settings found — launch the game once"))?;
    let active = profiles
        .active()
        .ok_or_else(|| anyhow::anyhow!("Siege settings folder exists but holds no profile"))?
        .clone();
    let store = optea_game::BackupStore::for_profile(&active.id)?;
    let file = optea_game::GuardedFile::new(active.settings_path.clone(), store);
    Ok((active, file))
}

fn cmd_siege(args: &Args) -> Result<()> {
    match args.positional.first().map(String::as_str) {
        Some("status") | None => {
            let (profile, file) = siege_file()?;
            render::siege_status(&profile, &file);
            Ok(())
        }
        Some("settings") => {
            let (_, file) = siege_file()?;
            let doc = optea_game::ini::IniDocument::parse(&file.read()?);
            let settings = optea_game::settings::GraphicsSettings::from_document(&doc);

            // Resolve the display the game most likely runs on: the primary.
            let displays = optea_sys::display::enumerate().unwrap_or_default();
            let primary = displays.iter().find(|d| d.is_primary).or(displays.first());
            let cpu = optea_sys::CpuInfo::query()?;
            let ctx = optea_game::settings::MachineContext {
                physical_cores: cpu.physical_cores,
                display_width: primary.map(|d| d.width as i64),
                display_height: primary.map(|d| d.height as i64),
                display_refresh_hz: primary.map(|d| d.refresh_hz),
            };

            let findings = optea_game::settings::analyze(&settings, &ctx);
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "settings": settings,
                        "findings": findings,
                    }))?
                );
            } else {
                render::siege_settings(&settings, &findings, &ctx);
            }
            Ok(())
        }
        Some("editable") => {
            render::siege_editable();
            Ok(())
        }
        Some("benchmark") => {
            let dir = optea_game::benchmark::report_dir()?;
            let report = optea_game::benchmark::latest_report(&dir)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render::siege_benchmark(&report);
            }
            Ok(())
        }
        Some("set") => {
            let name = args
                .positional
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: optea siege set <setting> <value>"))?;
            let raw = args
                .positional
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: optea siege set <setting> <value>"))?;

            let setting = optea_game::settings::find_editable(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "'{name}' is not a setting OPTEA will write — see `optea siege editable`"
                )
            })?;
            let value: i64 = raw
                .parse()
                .map_err(|_| anyhow::anyhow!("'{raw}' is not an integer"))?;
            if !setting.allowed.permits(value) {
                bail!(
                    "{} does not accept {value}. Allowed: {}",
                    setting.key,
                    setting.allowed.describe()
                );
            }

            let (_, file) = siege_file()?;
            let before = optea_game::ini::IniDocument::parse(&file.read()?);
            let current = before
                .get(setting.section, setting.key)
                .unwrap_or("<absent>")
                .to_string();

            if args.dry_run {
                println!(
                    "would set [{}] {} : {current} → {value} (no change made)",
                    setting.section, setting.key
                );
                return Ok(());
            }

            // Every write goes through GuardedFile, so a verified backup exists
            // before the transform is even invoked.
            let report = file.edit(|text| {
                let mut doc = optea_game::ini::IniDocument::parse(text);
                if !doc.set(setting.section, setting.key, &value.to_string()) {
                    return Err(format!(
                        "[{}] {} not present in this file",
                        setting.section, setting.key
                    ));
                }
                Ok(doc.to_string())
            })?;

            render::siege_set(setting, &current, value, &report);
            Ok(())
        }
        Some("backup") => {
            let (_, file) = siege_file()?;
            // Deliberately not gated on preflight: taking a backup only reads
            // the file, so it is safe even mid-match.
            let backup = file.store().ensure_pristine(file.target())?;
            backup.verify()?;
            let rolling = file.store().take(file.target())?;
            rolling.verify()?;
            println!("pristine : {}", backup.data_path.display());
            println!("backup   : {}", rolling.data_path.display());
            println!("sha256   : {}", rolling.meta.sha256);
            println!("verified : yes ({} bytes)", rolling.meta.size);
            Ok(())
        }
        Some("restore") => {
            let (_, file) = siege_file()?;
            match args.positional.get(1).map(String::as_str) {
                Some("pristine") | None => {
                    file.preflight()?;
                    file.restore_pristine()?;
                    println!("restored {} from the pristine copy", file.target().display());
                }
                Some(id) => {
                    file.preflight()?;
                    let backup = file
                        .store()
                        .history()
                        .into_iter()
                        .find(|b| b.meta.id == id)
                        .ok_or_else(|| anyhow::anyhow!("no backup with id '{id}'"))?;
                    file.restore(&backup)?;
                    println!("restored {} from {id}", file.target().display());
                }
            }
            Ok(())
        }
        Some(other) => bail!("unknown siege subcommand '{other}' (status | backup | restore)"),
    }
}

/// Executables that actually present frames, in priority order.
///
/// `RainbowSix_BE.exe` is deliberately absent: it is the BattlEye launcher
/// shim, which has no window and never presents. Tracking it yields a capture
/// with zero frames.
const KNOWN_GAMES: &[&str] = &["RainbowSix", "RainbowSixGame"];

fn cmd_capture(args: &Args) -> Result<()> {
    

    let pid = match args.pid {
        Some(p) => p,
        None => detect_game_pid()
            .ok_or_else(|| anyhow::anyhow!("no known game running — pass --pid <n>"))?,
    };

    println!(
        "capturing pid {pid} for {}s (passive ETW — the game is not touched)...",
        args.seconds
    );

    let capture = capture_with_focus(args, pid)?;

    let summary = optea_metrics::summarize(&capture.frames).ok_or_else(|| {
        anyhow::anyhow!(
            "captured {} frames but none usable",
            capture.frames.len()
        )
    })?;
    render::summary(&summary, &capture);
    Ok(())
}

/// Find a running game by executable name.
///
/// Iterates [`KNOWN_GAMES`] in priority order rather than walking the process
/// list, so a lower-priority match cannot win merely by appearing earlier in
/// `tasklist` output — which is how the BattlEye shim was picked over the game.
fn detect_game_pid() -> Option<u32> {
    let out = std::process::Command::new("tasklist")
        .args(["/fo", "csv", "/nh"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);

    let running: Vec<(String, u32)> = text
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split("\",\"").collect();
            if fields.len() < 2 {
                return None;
            }
            let name = fields[0].trim_start_matches('"');
            let stem = name.strip_suffix(".exe").unwrap_or(name);
            Some((stem.to_string(), fields[1].trim().parse().ok()?))
        })
        .collect();

    KNOWN_GAMES.iter().find_map(|game| {
        running
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(game))
            .map(|(_, pid)| *pid)
    })
}

fn cmd_list(args: &Args) -> Result<()> {
    let sys = SystemInfo::query()?;
    let catalog = optea_core::catalog::all(&sys);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&render::catalog_json(&catalog, &sys))?);
    } else {
        render::catalog(&catalog, &sys);
    }
    Ok(())
}

/// Resolve the tweaks a command should operate on.
fn select(args: &Args, sys: &SystemInfo) -> Result<Vec<Box<dyn Tweak>>> {
    if args.only.is_empty() {
        return Ok(optea_core::catalog::by_max_risk(sys, args.risk));
    }
    let mut out = Vec::new();
    for id in &args.only {
        match optea_core::catalog::find(sys, id) {
            Some(t) => out.push(t),
            None => bail!("no tweak with id '{id}' — run `optea list` to see them"),
        }
    }
    Ok(out)
}

fn cmd_apply(args: &Args) -> Result<()> {
    let sys = SystemInfo::query()?;
    let selected = select(args, &sys)?;

    // Refuse deep-risk work without the explicit acknowledgement, before doing
    // anything else.
    let deep: Vec<&str> = selected
        .iter()
        .filter(|t| t.risk() == Risk::Deep)
        .map(|t| t.id())
        .collect();
    if !deep.is_empty() && !args.understand {
        bail!(
            "these are deep-risk tweaks and can leave the machine unbootable: {}\n\
             Re-run with --i-understand once you have a restore point.",
            deep.join(", ")
        );
    }

    if args.dry_run {
        render::dry_run(&selected, &sys);
        return Ok(());
    }

    if !optea_sys::sysinfo::is_elevated()? {
        bail!("applying tweaks requires an elevated (administrator) terminal");
    }

    let engine = optea_core::Engine::with_default_dir(sys)?.allow_deep(args.understand);
    let refs: Vec<&dyn Tweak> = selected.iter().map(|b| b.as_ref()).collect();
    let result = engine.apply("cli", &refs)?;
    render::apply_result(&result, engine.snapshot_dir());
    Ok(())
}

fn cmd_revert(args: &Args) -> Result<()> {
    let sys = SystemInfo::query()?;
    let engine = optea_core::Engine::with_default_dir(sys.clone())?.allow_deep(true);

    let id = match args.positional.first() {
        Some(id) => id.clone(),
        None => match engine.latest_transaction() {
            Some(id) => id,
            None => {
                println!("nothing to revert — no transactions recorded");
                return Ok(());
            }
        },
    };

    if !optea_sys::sysinfo::is_elevated()? {
        bail!("reverting requires an elevated (administrator) terminal");
    }

    // Revert needs the full catalog, since a transaction may include deep entries.
    let catalog = optea_core::catalog::all(&sys);
    let refs: Vec<&dyn Tweak> = catalog.iter().map(|b| b.as_ref()).collect();
    let reverted = engine.revert(&id, &refs)?;

    if reverted.is_empty() {
        println!("transaction {id} had nothing applied to revert");
    } else {
        println!("reverted {} tweak(s) from {id}:", reverted.len());
        for r in reverted {
            println!("  {r}");
        }
    }
    Ok(())
}

fn cmd_history(args: &Args) -> Result<()> {
    let sys = SystemInfo::query()?;
    let engine = optea_core::Engine::with_default_dir(sys)?;
    let mut ids = engine.list_transactions()?;
    ids.sort();

    if args.json {
        let txs: Vec<_> = ids.iter().filter_map(|id| engine.load(id).ok()).collect();
        println!("{}", serde_json::to_string_pretty(&txs)?);
        return Ok(());
    }

    if ids.is_empty() {
        println!("no transactions recorded");
        return Ok(());
    }
    for id in ids {
        match engine.load(&id) {
            Ok(tx) => {
                let applied = tx.applied_ids();
                let state = if tx.reverted {
                    "reverted"
                } else if applied.is_empty() {
                    "nothing applied"
                } else {
                    "active"
                };
                println!("{id}  [{state}]  {}", applied.join(", "));
            }
            Err(e) => println!("{id}  <unreadable: {e}>"),
        }
    }
    Ok(())
}
