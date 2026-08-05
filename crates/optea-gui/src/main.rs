//! OPTEA desktop interface.
//!
//! A shell over `optea-core`; every decision it presents is made in the library
//! and shared with the CLI. The window's job is to make state legible and the
//! undo path obvious — not to hold logic of its own.

// Release builds open no console window; debug builds keep one for panics.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod state;
mod views;

use eframe::egui;
use state::{Job, Snapshot, Update, Worker};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 720.0])
            .with_min_inner_size([820.0, 560.0])
            .with_title("OPTEA — Rainbow Six Siege tuning"),
        ..Default::default()
    };

    eframe::run_native("OPTEA", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Tab {
    Dashboard,
    Settings,
    Background,
    Benchmarks,
}

pub struct App {
    worker: Worker,
    pub snapshot: Option<Snapshot>,
    pub tab: Tab,
    pub status: String,
    pub error: Option<String>,
    /// True between dispatching a job and its result arriving.
    pub busy: bool,
    /// Processes ticked in the Background tab.
    pub selected: std::collections::HashSet<String>,
    pub force: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        cc.egui_ctx.set_style(style);

        App {
            worker: Worker::spawn(cc.egui_ctx.clone()),
            snapshot: None,
            tab: Tab::Dashboard,
            status: "loading…".into(),
            error: None,
            busy: true,
            selected: Default::default(),
            force: false,
        }
    }

    pub fn dispatch(&mut self, job: Job) {
        self.busy = true;
        self.error = None;
        self.status = "working…".into();
        self.worker.send(job);
    }

    fn drain(&mut self) {
        for update in self.worker.poll() {
            match update {
                Update::Snapshot(s) => {
                    // Default the Background selection to the tier that is safe
                    // to close without asking.
                    if self.snapshot.is_none() {
                        self.selected = s
                            .candidates
                            .iter()
                            .filter(|c| c.tier == optea_core::quiet::Tier::Auto)
                            .map(|c| c.process.clone())
                            .collect();
                    }
                    self.snapshot = Some(*s);
                    self.busy = false;
                }
                Update::Done(msg) => {
                    self.status = msg;
                    self.error = None;
                }
                Update::Failed(msg) => {
                    self.status = "failed".into();
                    self.error = Some(msg);
                    self.busy = false;
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("OPTEA");
                ui.add_space(16.0);
                for (tab, label) in [
                    (Tab::Dashboard, "Dashboard"),
                    (Tab::Settings, "Game settings"),
                    (Tab::Background, "Background apps"),
                    (Tab::Benchmarks, "Benchmarks"),
                ] {
                    if ui.selectable_label(self.tab == tab, label).clicked() {
                        self.tab = tab;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Refresh"))
                        .clicked()
                    {
                        self.dispatch(Job::Refresh);
                    }
                    if self.busy {
                        ui.spinner();
                    }
                });
            });
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                match &self.error {
                    Some(e) => {
                        ui.colored_label(views::RED, "✖");
                        ui.colored_label(views::RED, e);
                    }
                    None => {
                        ui.label(egui::RichText::new(&self.status).weak());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(s) = &self.snapshot {
                        if !s.elevated {
                            ui.label(
                                egui::RichText::new("not elevated — system tweaks unavailable")
                                    .color(views::AMBER)
                                    .small(),
                            );
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.snapshot.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("gathering system state…");
                });
                return;
            }
            // Views draw from an immutable borrow and hand back what the user
            // asked for; the mutations happen here, once the borrow is over.
            let actions = egui::ScrollArea::vertical()
                .show(ui, |ui| match self.tab {
                    Tab::Dashboard => views::dashboard(self, ui),
                    Tab::Settings => views::settings(self, ui),
                    Tab::Background => views::background(self, ui),
                    Tab::Benchmarks => views::benchmarks(self, ui),
                })
                .inner;

            for action in actions {
                match action {
                    views::Action::Dispatch(job) => self.dispatch(job),
                    views::Action::ToggleForce(v) => self.force = v,
                    views::Action::Select(process, on) => {
                        if on {
                            self.selected.insert(process);
                        } else {
                            self.selected.remove(&process);
                        }
                    }
                }
            }
        });
    }
}
