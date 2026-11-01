//! Updates from GitHub: looked for once at start, installed when the rider asks. Only the
//! programs are downloaded; the new version runs from the next start.
use crate::{ACCENT, MUTED, WARN};
use bike_update::{Release, Target};
use eframe::egui::{self, RichText};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

#[derive(Debug, Clone)]
enum State {
    Idle,
    Checking,
    Latest,
    CheckFailed(String),
    Available(Release),
    Installing(String),
    Installed(String),
    InstallFailed(String),
}

pub struct Updater {
    state: Arc<Mutex<State>>,
    /// Resolved at start; `Err` explains why this installation cannot update itself.
    targets: Result<Vec<Target>, String>,
    /// The running program, to start it again after an update.
    exe: Option<PathBuf>,
    restart: bool,
    ctx: egui::Context,
}

impl Updater {
    /// `check` looks for a new version right away.
    pub fn new(ctx: egui::Context, check: bool) -> Self {
        let targets = bike_update::targets().map_err(|e| format!("{e:#}"));
        if let Ok(targets) = &targets {
            bike_update::remove_leftovers(targets);
        }
        let updater = Self {
            state: Arc::new(Mutex::new(State::Idle)),
            targets,
            exe: std::env::current_exe().ok(),
            restart: false,
            ctx,
        };
        if check && updater.targets.is_ok() {
            updater.check();
        }
        updater
    }

    fn state(&self) -> State {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sets the state now and runs `job` in the background; its result becomes the next state.
    fn run(&self, now: State, job: impl FnOnce() -> State + Send + 'static) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = now;
        let (state, ctx) = (self.state.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let next = job();
            *state.lock().unwrap_or_else(PoisonError::into_inner) = next;
            ctx.request_repaint();
        });
    }

    fn check(&self) {
        self.run(State::Checking, || match bike_update::latest() {
            Ok(release) if release.is_newer() => State::Available(release),
            Ok(_) => State::Latest,
            Err(e) => State::CheckFailed(format!("{e:#}")),
        });
    }

    fn install(&self, release: Release) {
        let Ok(targets) = self.targets.clone() else {
            return;
        };
        let version = release.version.clone();
        self.run(
            State::Installing(version.clone()),
            move || match bike_update::install(&release, &targets) {
                Ok(()) => State::Installed(version),
                Err(e) => State::InstallFailed(format!("{e:#}")),
            },
        );
    }

    /// A line under the tabs while an update is available, installing or installed.
    pub fn banner(&mut self, ui: &mut egui::Ui, running: bool) {
        let state = self.state();
        if !matches!(
            state,
            State::Available(_)
                | State::Installing(_)
                | State::Installed(_)
                | State::InstallFailed(_)
        ) {
            return;
        }
        ui.horizontal(|ui| self.actions(ui, state, running));
    }

    /// The Settings tab's version line.
    pub fn settings(&mut self, ui: &mut egui::Ui, running: bool) {
        ui.horizontal(|ui| {
            ui.label(format!("Version {}", bike_update::CURRENT));
            if let Err(why) = &self.targets {
                ui.label(RichText::new(format!("Updates: {why}")).color(MUTED));
                return;
            }
            let state = self.state();
            match &state {
                State::Idle | State::Latest | State::CheckFailed(_) => {
                    if ui.button("Check for updates").clicked() {
                        self.check();
                    }
                    match state {
                        State::Latest => {
                            ui.label(RichText::new("This is the latest version.").color(ACCENT));
                        }
                        State::CheckFailed(e) => {
                            ui.label(RichText::new(e).color(WARN));
                        }
                        _ => {}
                    }
                }
                State::Checking => {
                    ui.spinner();
                    ui.label(RichText::new("Checking...").color(MUTED));
                }
                _ => self.actions(ui, state, running),
            }
        });
    }

    fn actions(&mut self, ui: &mut egui::Ui, state: State, running: bool) {
        match state {
            State::Available(release) => {
                ui.label(
                    RichText::new(format!("Version {} is available.", release.version))
                        .color(ACCENT),
                );
                if ui
                    .add_enabled(!running, egui::Button::new("Update"))
                    .on_hover_text("Downloads only the new programs (a few MB)")
                    .on_disabled_hover_text("Stop the ride first")
                    .clicked()
                {
                    self.install(release);
                }
            }
            State::Installing(version) => {
                ui.spinner();
                ui.label(format!("Downloading version {version}..."));
            }
            State::Installed(version) => {
                ui.label(RichText::new(format!("Updated to {version}.")).color(ACCENT));
                if ui
                    .add_enabled(!running, egui::Button::new("Restart now"))
                    .on_disabled_hover_text("Stop the ride first")
                    .clicked()
                {
                    self.restart = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            State::InstallFailed(e) => {
                ui.label(RichText::new(format!("Update failed: {e}")).color(WARN));
                if ui.button("Try again").clicked() {
                    self.check();
                }
            }
            _ => {}
        }
    }

    /// Starts the new version after "Restart now"; call once the settings are saved.
    pub fn relaunch(&self) {
        if let (true, Some(exe)) = (self.restart, &self.exe)
            && let Err(e) = std::process::Command::new(exe).spawn()
        {
            tracing::warn!(error = %e, "cannot start the updated OsmolControl");
        }
    }
}
