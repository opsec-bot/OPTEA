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

Applying or reverting requires an elevated (administrator) terminal.
";

struct Args {
    command: String,
    json: bool,
    dry_run: bool,
    understand: bool,
    risk: Risk,
    only: Vec<String>,
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
