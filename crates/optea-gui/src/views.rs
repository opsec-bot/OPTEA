//! Tab rendering.
//!
//! The interface deliberately leads with what is *already correct* and what
//! cannot work, not just with a list of things to change. A tuning tool that
//! only ever shows problems teaches the user to distrust it.

use crate::state::Job;
use crate::App;
use eframe::egui::{self, Color32, RichText, Ui};

/// Views render from an immutable borrow of the snapshot and return the action
/// the user asked for, rather than mutating `App` mid-draw. Dispatching inside
/// the draw closure would need `&mut App` while the snapshot is still borrowed.
pub enum Action {
    Dispatch(Job),
    ToggleForce(bool),
    Select(String, bool),
}

pub const GREEN: Color32 = Color32::from_rgb(120, 200, 130);
pub const AMBER: Color32 = Color32::from_rgb(230, 180, 90);
pub const RED: Color32 = Color32::from_rgb(230, 110, 110);
pub const BLUE: Color32 = Color32::from_rgb(120, 180, 230);

fn card(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(4.0);
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width() - 8.0);
            ui.label(RichText::new(title).strong().size(15.0));
            ui.add_space(6.0);
            add(ui);
        });
}

fn kv(ui: &mut Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).weak());
        ui.label(value);
    });
}

pub fn dashboard(app: &App, ui: &mut Ui) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(s) = &app.snapshot else {
        return actions;
    };

    // ---- The single most important control -------------------------------
    let change_count = s.plan.as_ref().map(|p| p.changes.len()).unwrap_or(0);
    let blockers: Vec<String> = s
        .plan
        .as_ref()
        .map(|p| p.blockers.clone())
        .unwrap_or_default();
    let mut force = app.force;

    card(ui, "Optimise", |ui| {
        if change_count == 0 {
            ui.colored_label(GREEN, "Everything OPTEA would change is already applied.");
        } else {
            ui.label(format!(
                "{change_count} change(s) available: game settings, safe system tweaks, and \
                 background apps."
            ));
        }

        for b in &blockers {
            ui.add_space(4.0);
            ui.colored_label(AMBER, format!("⚠ {b}"));
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can_run = change_count > 0 && s.game_running.is_none() && !app.busy;
            let button = egui::Button::new(RichText::new("Apply optimisations").size(15.0))
                .fill(if can_run {
                    Color32::from_rgb(40, 90, 60)
                } else {
                    Color32::from_gray(50)
                });
            if ui.add_enabled(can_run, button).clicked() {
                actions.push(Action::Dispatch(Job::Optimize { force }));
            }
            if ui
                .checkbox(&mut force, "Force-close stubborn processes")
                .on_hover_text(
                    "Terminates background processes that have no window to ask. Discards \
                     unsaved work.",
                )
                .changed()
            {
                actions.push(Action::ToggleForce(force));
            }
        });

        if s.game_running.is_some() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Close Siege first — it rewrites GameSettings.ini on exit and would discard \
                     these changes.",
                )
                .color(AMBER)
                .small(),
            );
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("Undo").strong());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!app.busy, egui::Button::new("Restore game settings"))
                .on_hover_text(
                    "Restores GameSettings.ini to the copy taken before OPTEA first touched it.",
                )
                .clicked()
            {
                actions.push(Action::Dispatch(Job::RestoreSettings));
            }
            if ui
                .add_enabled(!app.busy, egui::Button::new("Revert system tweaks"))
                .on_hover_text("Reverts the most recent snapshot.")
                .clicked()
            {
                actions.push(Action::Dispatch(Job::RevertTweaks));
            }
        });
    });

    // ---- What the plan would actually do ---------------------------------
    if let Some(plan) = &s.plan {
        if !plan.is_empty() {
            card(ui, "Planned changes", |ui| {
                for (area, title) in [
                    ("game", "Game settings"),
                    ("system", "System"),
                    ("background", "Background apps"),
                ] {
                    let changes = plan.by_area(area);
                    if changes.is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(title).color(BLUE).strong());
                    for c in changes {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&c.what).strong());
                            ui.label(RichText::new(format!("{} → {}", c.from, c.to)).weak());
                            if c.needs_restart {
                                ui.label(RichText::new("needs game restart").color(AMBER).small());
                            }
                        });
                        ui.label(RichText::new(c.because).weak().small());
                        ui.add_space(4.0);
                    }
                    ui.add_space(4.0);
                }
            });
        }
    }

    // ---- Where the bottleneck actually is --------------------------------
    if let Some(b) = &s.benchmark {
        card(ui, "In-game benchmark — the engine's own measurement", |ui| {
            kv(ui, "Report", &b.stamp);
            kv(ui, "Average FPS", &format!("{:.1}", b.avg_fps));
            kv(
                ui,
                "Worst frame",
                &format!("{:.0} ms ({:.1} FPS)", b.largest_frame_time_ms, b.lowest_fps),
            );

            if let (Some(c50), Some(g50), Some(c95), Some(g95)) = (
                b.cpu_percentile(0.50),
                b.gpu_percentile(0.50),
                b.cpu_percentile(0.95),
                b.gpu_percentile(0.95),
            ) {
                ui.add_space(6.0);
                egui::Grid::new("cpugpu").num_columns(3).striped(true).show(ui, |ui| {
                    ui.label(RichText::new("").weak());
                    ui.label(RichText::new("CPU").strong());
                    ui.label(RichText::new("GPU").strong());
                    ui.end_row();
                    ui.label("median");
                    ui.label(format!("{c50:.2} ms"));
                    ui.label(format!("{g50:.2} ms"));
                    ui.end_row();
                    ui.label("p95");
                    ui.colored_label(if c95 > g95 { AMBER } else { GREEN }, format!("{c95:.2} ms"));
                    ui.label(format!("{g95:.2} ms"));
                    ui.end_row();
                });
            }

            match b.tail_is_cpu_bound() {
                Some(true) => {
                    ui.add_space(6.0);
                    ui.colored_label(AMBER, "The slow frames are CPU-bound.");
                    ui.label(
                        RichText::new(
                            "Lowering resolution, shadows or textures will not move the 1% lows \
                             here — those frames are not waiting on the GPU.",
                        )
                        .weak()
                        .small(),
                    );
                }
                Some(false) => {
                    ui.add_space(6.0);
                    ui.colored_label(BLUE, "The slow frames are GPU-bound.");
                }
                None => {}
            }
        });
    }

    // ---- System findings, best news first --------------------------------
    if let Some(report) = &s.report {
        card(ui, "System", |ui| {
            kv(ui, "CPU", &format!(
                "{} — {} cores / {} threads",
                report.cpu.name, report.cpu.physical_cores, report.cpu.logical_processors
            ));
            kv(ui, "OS", &report.os.version_string());
            for d in report.displays.iter().filter(|d| d.is_primary) {
                kv(ui, "Display", &format!("{} on {}", d.mode_string(), d.gpu_name));
            }
            kv(
                ui,
                "Frame capture",
                if s.presentmon {
                    "PresentMon available"
                } else {
                    "PresentMon not installed"
                },
            );

            ui.add_space(8.0);
            for f in &report.findings {
                use optea_core::doctor::Severity;
                let (colour, mark) = match f.severity {
                    Severity::Good => (GREEN, "✔"),
                    Severity::Info => (BLUE, "•"),
                    Severity::Warn => (AMBER, "⚠"),
                    Severity::Critical => (RED, "✖"),
                };
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(colour, mark);
                    ui.label(RichText::new(&f.title).strong());
                });
                ui.label(RichText::new(&f.detail).weak().small());
                if let Some(a) = &f.advice {
                    ui.label(RichText::new(format!("→ {a}")).weak().small());
                }
                ui.add_space(6.0);
            }
        });
    }

    actions
}

pub fn settings(app: &App, ui: &mut Ui) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(s) = &app.snapshot else {
        return actions;
    };
    let Some(g) = &s.settings else {
        ui.label("No Siege settings found. Launch the game once so it writes GameSettings.ini.");
        return actions;
    };

    card(ui, "Current settings", |ui| {
        egui::Grid::new("gs").num_columns(2).striped(true).show(ui, |ui| {
            ui.label("Render resolution");
            ui.label(g.resolution_label());
            ui.end_row();
            if let Some(m) = g.window_mode {
                ui.label("Window mode");
                ui.label(m.label());
                ui.end_row();
            }
            if let Some(r) = g.reflex {
                ui.label("NVIDIA Reflex");
                ui.label(r.label());
                ui.end_row();
            }
            if let Some(v) = g.vsync {
                ui.label("VSync");
                ui.label(if v == 0 { "off" } else { "on" });
                ui.end_row();
            }
            if let Some(b) = g.max_gpu_buffered_frame {
                ui.label("Buffered frames");
                ui.label(b.to_string());
                ui.end_row();
            }
            if let Some(f) = g.fps_limit {
                ui.label("FPS limit");
                ui.label(if f == 0 { "uncapped".into() } else { f.to_string() });
                ui.end_row();
            }
        });

        if !g.quality.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Quality").strong());
            ui.horizontal_wrapped(|ui| {
                for (k, v) in &g.quality {
                    ui.label(RichText::new(format!("{k}={v}")).weak().small());
                }
            });
        }
    });

    card(ui, "Analysis", |ui| {
        use optea_game::settings::Impact;
        for f in &s.findings {
            let (colour, mark) = match f.impact {
                Impact::Good => (GREEN, "✔"),
                Impact::Info => (BLUE, "•"),
                Impact::Opportunity => (AMBER, "test"),
            };
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(colour, mark);
                ui.label(RichText::new(&f.setting).strong());
                ui.label(RichText::new(format!("= {}", f.current)).weak());
            });
            ui.label(RichText::new(&f.detail).weak().small());
            if let Some(sg) = &f.suggestion {
                ui.label(RichText::new(format!("→ {sg}")).weak().small());
            }
            ui.add_space(8.0);
        }
    });

    card(ui, "Backups", |ui| {
        ui.label(
            RichText::new(
                "A pristine copy is taken before OPTEA ever writes to this file, and never \
                 overwritten. Every edit also takes its own timestamped backup, verified by \
                 SHA-256.",
            )
            .weak()
            .small(),
        );
        ui.add_space(6.0);
        if ui
            .add_enabled(!app.busy, egui::Button::new("Restore original settings"))
            .clicked()
        {
            actions.push(Action::Dispatch(Job::RestoreSettings));
        }
    });

    actions
}

pub fn background(app: &App, ui: &mut Ui) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(s) = &app.snapshot else {
        return actions;
    };

    if s.candidates.is_empty() {
        ui.label("No known background apps are running.");
        return actions;
    }

    let candidates: Vec<_> = s.candidates.clone();
    let mut force = app.force;

    card(ui, "Background applications", |ui| {
        ui.label(
            RichText::new(
                "Only apps on OPTEA's allowlist appear here. Anti-cheat, the game, the shell and \
                 system processes are never candidates.",
            )
            .weak()
            .small(),
        );
        ui.add_space(8.0);

        for c in &candidates {
            let mut checked = app.selected.contains(&c.process);
            ui.horizontal_wrapped(|ui| {
                if ui.checkbox(&mut checked, "").changed() {
                    actions.push(Action::Select(c.process.clone(), checked));
                }
                ui.label(RichText::new(&c.label).strong());
                ui.label(RichText::new(c.instance_note()).weak().small());
                if c.tier == optea_core::quiet::Tier::Ask {
                    ui.label(RichText::new("holds your work").color(AMBER).small());
                }
            });
            ui.label(RichText::new(&c.why).weak().small());
            if let Some(cost) = &c.cost {
                ui.label(RichText::new(format!("cost: {cost}")).color(AMBER).small());
            }
            ui.add_space(8.0);
        }

        ui.separator();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let n = app.selected.len();
            if ui
                .add_enabled(
                    n > 0 && !app.busy,
                    egui::Button::new(format!("Close {n} app(s)")),
                )
                .clicked()
            {
                let processes: Vec<String> = app.selected.iter().cloned().collect();
                actions.push(Action::Dispatch(Job::CloseApps { processes, force }));
            }
            if ui
                .checkbox(&mut force, "Force")
                .on_hover_text(
                    "Terminate processes that have no window to ask. Discards unsaved work.",
                )
                .changed()
            {
                actions.push(Action::ToggleForce(force));
            }
        });
        ui.label(
            RichText::new("OPTEA does not reopen these — start them again yourself afterwards.")
                .weak()
                .small(),
        );
    });

    actions
}

pub fn benchmarks(app: &App, ui: &mut Ui) -> Vec<Action> {
    let actions = Vec::new();
    let Some(s) = &app.snapshot else {
        return actions;
    };

    card(ui, "Recorded runs", |ui| {
        if s.labels.is_empty() {
            ui.label("Nothing recorded yet.");
        } else {
            egui::Grid::new("labels").num_columns(3).striped(true).show(ui, |ui| {
                for (label, count) in &s.labels {
                    ui.label(RichText::new(label).strong());
                    ui.label(format!("{count} run(s)"));
                    let (colour, note) = if *count < optea_core::bench::MIN_RUNS {
                        (RED, "too few to compare")
                    } else if *count < optea_core::bench::RECOMMENDED_RUNS {
                        (AMBER, "only a large effect would show")
                    } else {
                        (GREEN, "ready")
                    };
                    ui.colored_label(colour, note);
                    ui.end_row();
                }
            });
        }

        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "Recording a run drives the game's benchmark and needs the window focused, so it \
                 is done from the command line rather than here:",
            )
            .weak()
            .small(),
        );
        ui.add_space(4.0);
        ui.code("optea bench record --label baseline --wait 30 --delay 20 --seconds 70");
        ui.add_space(4.0);
        ui.code("optea bench compare baseline optimized");
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Five runs per label is where small effects become resolvable. A comparison is \
                 refused on a single run, on runs captured while the game was not focused, and on \
                 runs that used different capture windows.",
            )
            .weak()
            .small(),
        );
    });

    if !s.presentmon {
        card(ui, "PresentMon not installed", |ui| {
            ui.colored_label(AMBER, "Frame capture is unavailable, so tweaks cannot be verified.");
            ui.label(
                RichText::new(
                    "Install Intel PresentMon (service + SDK) from GameTechDev/PresentMon.",
                )
                .weak()
                .small(),
            );
        });
    }

    actions
}
