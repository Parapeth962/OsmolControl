//! Controllers tab: which shift controller to use, the supported ones, and learning a new one
//! from a few button presses (saved to the user's controllers.json).
use crate::backend::Backend;
use crate::{ACCENT, MUTED, WARN};
use bike_ble::Nearby;
use bike_core::ControllerProfile;
use bike_session::Settings;
use eframe::egui::{self, RichText};
use tokio::sync::{mpsc, oneshot, watch};

/// Presses asked for per button; learning needs at least two to see a repeat.
const PRESSES: usize = 3;
const SCAN_SECONDS: u64 = 6;

#[derive(Default)]
pub struct ControllersTab {
    learn: Option<Learn>,
    /// Outcome of the last save or removal; true for errors.
    message: Option<(String, bool)>,
}

struct Learn {
    step: Step,
    /// Stops the scan's follow-up connection when the wizard is closed.
    stop: watch::Sender<bool>,
    frames: Vec<(String, Vec<u8>)>,
    rx: Option<mpsc::UnboundedReceiver<(String, Vec<u8>)>>,
    done: Option<oneshot::Receiver<Result<(), String>>>,
}

enum Step {
    Scanning(oneshot::Receiver<Result<Vec<Nearby>, String>>),
    Pick(Vec<Nearby>),
    Connecting(String, oneshot::Receiver<()>),
    /// Pressing the harder button; frames from `from` on belong to it.
    Harder {
        name: String,
        from: usize,
    },
    Easier {
        name: String,
        harder: Vec<(String, Vec<u8>)>,
        from: usize,
    },
    Review {
        profile: ControllerProfile,
        prefix: String,
    },
    Failed(String),
}

impl Learn {
    fn start(backend: &Backend, adapter: usize) -> Self {
        let (tx, rx) = oneshot::channel();
        backend.spawn(async move {
            let found =
                async { bike_ble::nearby(&bike_ble::adapter(adapter).await?, SCAN_SECONDS).await };
            let _ = tx.send(found.await.map_err(|e| format!("{e:#}")));
        });
        Self {
            step: Step::Scanning(rx),
            stop: watch::channel(false).0,
            frames: Vec::new(),
            rx: None,
            done: None,
        }
    }

    fn connect(&mut self, backend: &Backend, device: &Nearby) {
        let (frames, rx) = mpsc::unbounded_channel();
        let (ready, connected) = oneshot::channel();
        let (done_tx, done) = oneshot::channel();
        let stopped = self.stop.subscribe();
        let peripheral = device.peripheral.clone();
        backend.spawn(async move {
            let result = bike_ble::listen_all(&peripheral, frames, ready, stopped).await;
            let _ = done_tx.send(result.map_err(|e| format!("{e:#}")));
        });
        self.rx = Some(rx);
        self.done = Some(done);
        self.step = Step::Connecting(device.name.clone(), connected);
    }

    /// Collect what the background jobs delivered and advance automatic steps.
    fn poll(&mut self) {
        if let Some(rx) = &mut self.rx {
            while let Ok(frame) = rx.try_recv() {
                self.frames.push(frame);
            }
        }
        if let Some(done) = &mut self.done
            && let Ok(result) = done.try_recv()
        {
            self.done = None;
            if !matches!(self.step, Step::Review { .. } | Step::Failed(_)) {
                let why = result
                    .err()
                    .unwrap_or_else(|| "the connection ended".into());
                self.step = Step::Failed(format!("Lost the controller: {why}"));
            }
        }
        match &mut self.step {
            Step::Scanning(rx) => match rx.try_recv() {
                Ok(Ok(found)) => self.step = Step::Pick(found),
                Ok(Err(e)) => self.step = Step::Failed(format!("Bluetooth search failed: {e}")),
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.step = Step::Failed("Bluetooth search stopped".into());
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            },
            Step::Connecting(name, connected) => {
                if connected.try_recv().is_ok() {
                    self.step = Step::Harder {
                        name: std::mem::take(name),
                        from: self.frames.len(),
                    };
                }
            }
            _ => {}
        }
    }
}

/// Frames received since index `from`.
fn since(frames: &[(String, Vec<u8>)], from: usize) -> &[(String, Vec<u8>)] {
    frames.get(from..).unwrap_or_default()
}

impl Drop for Learn {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl ControllersTab {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        backend: &Backend,
        running: bool,
    ) {
        if running {
            // A ride needs the Bluetooth adapter; learning would compete with its scans.
            self.learn = None;
            ui.label(RichText::new("Changes apply from the next ride.").color(WARN));
        }
        let builtin = bike_core::builtin_controllers();
        let (user, user_error) = match bike_session::user_controllers() {
            Ok(user) => (user, None),
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        };

        ui.label(RichText::new("Controller to use").strong());
        let any = "Any supported controller";
        let selected = settings.controller.clone().unwrap_or_else(|| any.into());
        egui::ComboBox::from_id_salt("controller")
            .selected_text(&selected)
            .width(340.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut settings.controller, None, any);
                for p in user.iter().chain(&builtin) {
                    ui.selectable_value(&mut settings.controller, Some(p.label.clone()), &p.label);
                }
            });
        ui.label(
            RichText::new(
                "Automatic uses the first supported controller that is switched on. Choose one \
                 when several are nearby.",
            )
            .color(MUTED),
        );
        ui.separator();

        ui.label(RichText::new("Supported controllers").strong());
        for p in &builtin {
            ui.label(format!("• {}", p.label));
        }
        let mut remove = None;
        for p in &user {
            ui.horizontal(|ui| {
                ui.label(format!("• {}", p.label));
                ui.label(RichText::new("yours").color(ACCENT));
                if ui.small_button("Remove").clicked() {
                    remove = Some(p.label.clone());
                }
            });
        }
        if let Some(label) = remove {
            self.message = Some(match bike_session::remove_controller(&label) {
                Ok(_) => (format!("Removed {label}."), false),
                Err(e) => (format!("{e:#}"), true),
            });
            if settings.controller.as_deref() == Some(label.as_str()) {
                settings.controller = None;
            }
        }
        if let Some(e) = user_error {
            ui.label(
                RichText::new(format!("Your controllers file cannot be used: {e}")).color(WARN),
            );
        }
        ui.label(
            RichText::new(
                "Zwift Click, Play and Ride, SRAM AXS and others with encrypted or two-way \
                 protocols are not supported yet.",
            )
            .color(MUTED),
        );
        ui.separator();

        ui.label(RichText::new("Learn a new controller").strong());
        match &mut self.learn {
            None => {
                ui.label(
                    "Works with controllers that send the same Bluetooth message every time a \
                     button is pressed. You press each shift button three times.",
                );
                let start = ui.add_enabled(!running, egui::Button::new("Learn a controller..."));
                if start
                    .on_disabled_hover_text("Stop the ride first")
                    .clicked()
                {
                    self.message = None;
                    self.learn = Some(Learn::start(backend, settings.adapter));
                }
            }
            Some(learn) => {
                learn.poll();
                // Keep the message counter live.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(150));
                let close = wizard(ui, learn, backend, settings, &mut self.message);
                if close {
                    self.learn = None;
                }
            }
        }
        if let Some((text, error)) = &self.message {
            let color = if *error { WARN } else { ACCENT };
            ui.label(RichText::new(text).color(color));
        }
        ui.separator();
        if let Some(path) = bike_session::controllers_file() {
            ui.label(
                RichText::new(format!(
                    "Learned controllers are saved in {}. The manual describes the format for \
                     editing it by hand.",
                    path.display()
                ))
                .color(MUTED),
            );
        }
    }
}

/// One wizard step; true when the wizard should close.
fn wizard(
    ui: &mut egui::Ui,
    learn: &mut Learn,
    backend: &Backend,
    settings: &mut Settings,
    message: &mut Option<(String, bool)>,
) -> bool {
    let mut close = false;
    let mut next = None;
    match &mut learn.step {
        Step::Scanning(_) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "Looking for Bluetooth devices ({SCAN_SECONDS} s)... press a button on the \
                     controller to wake it up."
                ));
            });
        }
        Step::Pick(found) => {
            if found.is_empty() {
                ui.label("No Bluetooth devices found. Wake the controller and search again.");
            } else {
                ui.label("Choose your controller:");
            }
            let profiles = bike_core::builtin_controllers();
            let mut chosen = None;
            for device in found.iter() {
                let known =
                    bike_core::controller_profile(&profiles, &device.name, &device.services);
                ui.horizontal(|ui| {
                    if ui.button(&device.name).clicked() {
                        chosen = Some(device.clone());
                    }
                    if let Some(rssi) = device.rssi {
                        ui.label(RichText::new(format!("{rssi} dBm")).color(MUTED));
                    }
                    if let Some(p) = known {
                        ui.label(
                            RichText::new(format!("already supported: {}", p.label)).color(ACCENT),
                        );
                    }
                });
            }
            if let Some(device) = chosen {
                learn.connect(backend, &device);
            } else if ui.button("Search again").clicked() {
                *learn = Learn::start(backend, settings.adapter);
            }
        }
        Step::Connecting(name, _) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Connecting to {name}..."));
            });
        }
        Step::Harder { name, from } => {
            let (name, from) = (name.clone(), *from);
            let received = since(&learn.frames, from).len();
            ui.label(
                RichText::new(format!(
                    "Press the HARDER (+) button {PRESSES} times, about a second apart."
                ))
                .size(16.0),
            );
            ui.label(RichText::new(format!("{received} message(s) received")).color(MUTED));
            if ui
                .add_enabled(received > 0, egui::Button::new("Next"))
                .clicked()
            {
                next = Some(Step::Easier {
                    name,
                    harder: since(&learn.frames, from).to_vec(),
                    from: learn.frames.len(),
                });
            }
        }
        Step::Easier { name, harder, from } => {
            let from = *from;
            let received = since(&learn.frames, from).len();
            ui.label(
                RichText::new(format!(
                    "Now press the EASIER (−) button {PRESSES} times, about a second apart."
                ))
                .size(16.0),
            );
            ui.label(RichText::new(format!("{received} message(s) received")).color(MUTED));
            if ui
                .add_enabled(received > 0, egui::Button::new("Next"))
                .clicked()
            {
                let label = bike_core::name_prefix(name).trim().to_owned();
                next = Some(
                    match bike_core::learn_profile(&label, name, harder, since(&learn.frames, from))
                    {
                        Ok(profile) => {
                            let prefix = profile.name_prefixes.concat();
                            Step::Review { profile, prefix }
                        }
                        Err(e) => Step::Failed(e),
                    },
                );
                // Nothing more to listen for.
                let _ = learn.stop.send(true);
            }
        }
        Step::Review { profile, prefix } => {
            ui.label(RichText::new("Learned it.").color(ACCENT).size(16.0));
            egui::Grid::new("learned").num_columns(2).show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut profile.label);
                ui.end_row();
                ui.label("Device names starting with");
                ui.text_edit_singleline(prefix);
                ui.end_row();
                ui.label("Harder");
                ui.label(RichText::new(&profile.shift_up_hex).monospace());
                ui.end_row();
                ui.label("Easier");
                ui.label(RichText::new(&profile.shift_down_hex).monospace());
                ui.end_row();
            });
            if ui.button("Save and use it").clicked() {
                let mut profile = profile.clone();
                profile.label = profile.label.trim().to_owned();
                profile.name_prefixes = vec![prefix.trim().to_owned()];
                let label = profile.label.clone();
                *message = Some(match bike_session::save_controller(profile) {
                    Ok(_) => {
                        settings.controller = Some(label.clone());
                        close = true;
                        (
                            format!("Saved {label}; it is used from the next ride."),
                            false,
                        )
                    }
                    Err(e) => (format!("Not saved: {e:#}"), true),
                });
            }
        }
        Step::Failed(why) => {
            ui.label(RichText::new(why.as_str()).color(WARN));
            if ui.button("Try again").clicked() {
                *learn = Learn::start(backend, settings.adapter);
            }
        }
    }
    if let Some(step) = next {
        learn.step = step;
    }
    if !close && ui.button("Cancel").clicked() {
        close = true;
    }
    close
}
