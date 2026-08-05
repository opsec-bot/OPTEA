//! Application state, and the background worker that keeps the UI responsive.
//!
//! Every operation OPTEA performs touches the system: enumerating processes,
//! reading the registry, closing applications, running a 70-second capture.
//! None of that belongs on the UI thread — a frozen window during a benchmark
//! would look like a crash. Work is dispatched to a worker thread and results
//! come back over a channel, so the window keeps painting throughout.

use optea_core::optimize::Plan;
use optea_core::quiet::Candidate;
use optea_core::tweak::SystemInfo;
use std::sync::mpsc::{Receiver, Sender};

/// A job for the worker thread.
pub enum Job {
    Refresh,
    Optimize { force: bool },
    CloseApps { processes: Vec<String>, force: bool },
    RestoreSettings,
    RevertTweaks,
}

/// A result coming back from the worker.
pub enum Update {
    Snapshot(Box<Snapshot>),
    /// Something finished; the string is shown in the status bar.
    Done(String),
    Failed(String),
}

/// Everything the UI draws, gathered in one pass off the UI thread.
pub struct Snapshot {
    pub report: Option<optea_core::doctor::Report>,
    pub plan: Option<Plan>,
    pub settings: Option<optea_game::settings::GraphicsSettings>,
    pub findings: Vec<optea_game::settings::SettingFinding>,
    pub benchmark: Option<optea_game::benchmark::BenchmarkReport>,
    pub candidates: Vec<Candidate>,
    pub labels: std::collections::BTreeMap<String, usize>,
    pub game_running: Option<u32>,
    pub elevated: bool,
    pub presentmon: bool,
}

impl Snapshot {
    /// Gather current state. Slow — call only from the worker thread.
    pub fn gather() -> Self {
        let sys = SystemInfo::query().ok();
        let report = optea_core::doctor::run().ok();
        let plan = sys.as_ref().map(optea_core::optimize::plan);

        let (settings, findings) = match load_settings() {
            Some(pair) => (Some(pair.0), pair.1),
            None => (None, Vec::new()),
        };

        let benchmark = optea_game::benchmark::report_dir()
            .ok()
            .and_then(|d| optea_game::benchmark::latest_report(&d).ok());

        let labels = optea_core::bench::BenchStore::with_default_dir()
            .map(|s| s.labels())
            .unwrap_or_default();

        Snapshot {
            report,
            plan,
            settings,
            findings,
            benchmark,
            candidates: optea_core::quiet::candidates(),
            labels,
            game_running: optea_game::running_game_pid(),
            elevated: optea_sys::sysinfo::is_elevated().unwrap_or(false),
            presentmon: optea_metrics::presentmon::is_installed(),
        }
    }
}

fn load_settings() -> Option<(
    optea_game::settings::GraphicsSettings,
    Vec<optea_game::settings::SettingFinding>,
)> {
    let profiles = optea_game::profile::discover().ok()??;
    let active = profiles.active()?;
    let text = std::fs::read_to_string(&active.settings_path).ok()?;
    let doc = optea_game::ini::IniDocument::parse(&text);
    let settings = optea_game::settings::GraphicsSettings::from_document(&doc);

    let displays = optea_sys::display::enumerate().unwrap_or_default();
    let primary = displays.iter().find(|d| d.is_primary).or(displays.first());
    let cpu = optea_sys::CpuInfo::query().ok()?;
    let ctx = optea_game::settings::MachineContext {
        physical_cores: cpu.physical_cores,
        display_width: primary.map(|d| d.width as i64),
        display_height: primary.map(|d| d.height as i64),
        display_refresh_hz: primary.map(|d| d.refresh_hz),
    };

    let findings = optea_game::settings::analyze(&settings, &ctx);
    Some((settings, findings))
}

/// Handle to the worker thread.
pub struct Worker {
    jobs: Sender<Job>,
    updates: Receiver<Update>,
}

impl Worker {
    pub fn spawn(ctx: eframe::egui::Context) -> Self {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
        let (up_tx, up_rx) = std::sync::mpsc::channel::<Update>();

        std::thread::spawn(move || {
            // Seed the UI with a first snapshot before waiting for input.
            let _ = up_tx.send(Update::Snapshot(Box::new(Snapshot::gather())));
            ctx.request_repaint();

            while let Ok(job) = job_rx.recv() {
                let result = run_job(job);
                let _ = up_tx.send(result);
                // A fresh snapshot after every job, so the UI never shows a
                // stale plan next to a button that has already been pressed.
                let _ = up_tx.send(Update::Snapshot(Box::new(Snapshot::gather())));
                ctx.request_repaint();
            }
        });

        Worker {
            jobs: job_tx,
            updates: up_rx,
        }
    }

    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    pub fn poll(&self) -> Vec<Update> {
        self.updates.try_iter().collect()
    }
}

fn run_job(job: Job) -> Update {
    match job {
        Job::Refresh => Update::Done("refreshed".into()),

        Job::Optimize { force } => match apply_optimize(force) {
            Ok(n) => Update::Done(format!("{n} change(s) applied")),
            Err(e) => Update::Failed(e.to_string()),
        },

        Job::CloseApps { processes, force } => {
            let selected: Vec<Candidate> = optea_core::quiet::candidates()
                .into_iter()
                .filter(|c| processes.iter().any(|p| p.eq_ignore_ascii_case(&c.process)))
                .collect();
            let results = optea_core::quiet::close_all(&selected, force);
            let closed = results.iter().filter(|r| r.fully_closed()).count();
            let stuck: usize = results.iter().map(|r| r.no_window + r.declined).sum();
            if stuck > 0 {
                Update::Done(format!(
                    "{closed} closed, {stuck} process(es) could not be asked — use Force"
                ))
            } else {
                Update::Done(format!("{closed} app(s) closed"))
            }
        }

        Job::RestoreSettings => match restore_settings() {
            Ok(()) => Update::Done("game settings restored to the original".into()),
            Err(e) => Update::Failed(e.to_string()),
        },

        Job::RevertTweaks => match revert_tweaks() {
            Ok(n) => Update::Done(format!("{n} system tweak(s) reverted")),
            Err(e) => Update::Failed(e.to_string()),
        },
    }
}

fn siege_file() -> anyhow::Result<optea_game::GuardedFile> {
    let profiles = optea_game::profile::discover()?
        .ok_or_else(|| anyhow::anyhow!("no Siege settings found"))?;
    let active = profiles
        .active()
        .ok_or_else(|| anyhow::anyhow!("no Siege profile"))?;
    let store = optea_game::BackupStore::for_profile(&active.id)?;
    Ok(optea_game::GuardedFile::new(
        active.settings_path.clone(),
        store,
    ))
}

fn apply_optimize(force: bool) -> anyhow::Result<usize> {
    use optea_core::tweak::{Risk, Tweak};

    if let Some(pid) = optea_game::running_game_pid() {
        anyhow::bail!(
            "Siege is running (pid {pid}) and rewrites GameSettings.ini on exit. Close it first."
        );
    }

    let sys = SystemInfo::query()?;
    let plan = optea_core::optimize::plan(&sys);
    let mut applied = 0;

    // Game settings, each through the verified-backup path.
    if !plan.by_area("game").is_empty() {
        let file = siege_file()?;
        for change in plan.by_area("game") {
            let Some(step) = optea_core::optimize::SETTING_PLAN.iter().find(|s| {
                optea_game::settings::find_editable(s.alias).is_some_and(|e| e.key == change.what)
            }) else {
                continue;
            };
            let setting = optea_game::settings::find_editable(step.alias).unwrap();
            let value = step.target.to_string();
            let report = file.edit(|text| {
                let mut doc = optea_game::ini::IniDocument::parse(text);
                if !doc.set(setting.section, setting.key, &value) {
                    return Err(format!("{} not present", setting.key));
                }
                Ok(doc.to_string())
            })?;
            if report.changed {
                applied += 1;
            }
        }
    }

    // System tweaks, only when elevated.
    if optea_sys::sysinfo::is_elevated().unwrap_or(false) {
        let catalog = optea_core::catalog::by_max_risk(&sys, Risk::Safe);
        let refs: Vec<&dyn Tweak> = catalog.iter().map(|b| b.as_ref()).collect();
        let engine = optea_core::Engine::with_default_dir(sys.clone())?;
        let result = engine.apply("gui", &refs)?;
        applied += result
            .outcomes
            .values()
            .filter(|o| matches!(o, optea_core::Outcome::Applied))
            .count();
    }

    // Background apps, auto tier only.
    let auto: Vec<Candidate> = optea_core::quiet::candidates()
        .into_iter()
        .filter(|c| c.tier == optea_core::quiet::Tier::Auto)
        .collect();
    for r in optea_core::quiet::close_all(&auto, force) {
        if r.closed > 0 || r.forced > 0 {
            applied += 1;
        }
    }

    Ok(applied)
}

fn restore_settings() -> anyhow::Result<()> {
    let file = siege_file()?;
    file.preflight()?;
    file.restore_pristine()?;
    Ok(())
}

fn revert_tweaks() -> anyhow::Result<usize> {
    use optea_core::tweak::Tweak;

    let sys = SystemInfo::query()?;
    let engine = optea_core::Engine::with_default_dir(sys.clone())?.allow_deep(true);
    let Some(id) = engine.latest_transaction() else {
        anyhow::bail!("no transaction to revert");
    };
    let catalog = optea_core::catalog::all(&sys);
    let refs: Vec<&dyn Tweak> = catalog.iter().map(|b| b.as_ref()).collect();
    Ok(engine.revert(&id, &refs)?.len())
}
