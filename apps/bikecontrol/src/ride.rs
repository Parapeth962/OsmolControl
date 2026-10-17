//! Ride tab: gear, live numbers, connections and what to do in MyWhoosh.
use crate::backend::Backend;
use crate::{ACCENT, MUTED, WARN};
use bike_session::{Control, Mode, Status};
use eframe::egui::{self, Color32, RichText};

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .corner_radius(8)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// One big number with a caption; `None` shows a dash, never a made-up zero.
fn metric(ui: &mut egui::Ui, caption: &str, value: Option<String>, unit: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(caption).color(MUTED));
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(value.unwrap_or_else(|| "–".into()))
                    .size(30.0)
                    .strong(),
            );
            ui.label(RichText::new(unit).color(MUTED));
        });
    });
}

fn dot(ui: &mut egui::Ui, on: bool, label: &str, detail: Option<String>) {
    ui.horizontal(|ui| {
        let color = if on { ACCENT } else { Color32::from_gray(90) };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 5.0, color);
        ui.label(RichText::new(label).strong());
        if let Some(d) = detail {
            ui.label(RichText::new(d).color(MUTED));
        }
    });
}

fn shift_buttons(ui: &mut egui::Ui, backend: &Backend, running: bool) {
    ui.horizontal(|ui| {
        let size = egui::vec2(130.0, 40.0);
        if ui
            .add_enabled(
                running,
                egui::Button::new(RichText::new("−  Easier").size(18.0)).min_size(size),
            )
            .clicked()
        {
            backend.shift(false);
        }
        if ui
            .add_enabled(
                running,
                egui::Button::new(RichText::new("Harder  +").size(18.0)).min_size(size),
            )
            .clicked()
        {
            backend.shift(true);
        }
        ui.label(RichText::new("or Up / Down keys").color(MUTED));
    });
}

pub fn show(ui: &mut egui::Ui, status: &Status, backend: &Backend, running: bool) {
    if let Some(error) = &status.error {
        card(ui, |ui| {
            ui.label(RichText::new("Something went wrong").color(WARN).strong());
            ui.label(error);
        });
    }
    if !running && status.phase == bike_session::Phase::Idle {
        card(ui, |ui| {
            ui.label(RichText::new("Ready when you are").size(20.0).strong());
            ui.label("1. Switch on the trainer and the controller.");
            ui.label("2. Press ▶ Start ride. OsmolControl finds them automatically.");
            ui.label("3. Follow the steps shown here to pair OsmolControl in MyWhoosh.");
            ui.label(
                RichText::new("Pick your gears in the Gears tab and the mode in Settings.")
                    .color(MUTED),
            );
        });
        return;
    }

    card(ui, |ui| match (status.mode, status.gear) {
        (Mode::OsmolGears, Some(g)) => {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(g.number.to_string())
                        .size(64.0)
                        .strong()
                        .color(ACCENT),
                );
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(format!("of {}", g.of))
                            .size(18.0)
                            .color(MUTED),
                    );
                    ui.label(
                        RichText::new(format!("{} × {}", g.chainring, g.cog))
                            .size(22.0)
                            .strong(),
                    );
                    ui.label(RichText::new(format!("ratio {:.2}", g.ratio)).color(MUTED));
                });
                if let Some(name) = &status.gearing_name {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        ui.label(RichText::new(name).color(MUTED));
                    });
                }
            });
            if status.control == Control::Target {
                ui.label(
                    RichText::new("ERG workout: MyWhoosh sets the power, gears are paused.")
                        .color(WARN),
                );
            }
            shift_buttons(ui, backend, running);
        }
        _ => {
            ui.label(
                RichText::new("MyWhoosh does the shifting")
                    .size(18.0)
                    .strong(),
            );
            if let Some((action, at)) = status.last_action
                && at.elapsed().as_secs() < 3
            {
                let word = if action == bike_protocol::Action::ShiftUp {
                    "Harder"
                } else {
                    "Easier"
                };
                ui.label(RichText::new(word).size(22.0).color(ACCENT));
            }
            shift_buttons(ui, backend, running);
        }
    });

    card(ui, |ui| {
        let fresh = status.telemetry_fresh();
        egui::Grid::new("metrics")
            .num_columns(3)
            .spacing([28.0, 10.0])
            .show(ui, |ui| {
                metric(
                    ui,
                    "Power",
                    status.power_w.filter(|_| fresh).map(|w| w.to_string()),
                    "W",
                );
                metric(
                    ui,
                    "Cadence",
                    status
                        .cadence_rpm
                        .filter(|_| fresh)
                        .map(|c| format!("{c:.0}")),
                    "rpm",
                );
                metric(
                    ui,
                    "Speed",
                    status
                        .speed_kmh
                        .filter(|_| fresh)
                        .map(|v| format!("{v:.1}")),
                    "km/h",
                );
                ui.end_row();
                metric(
                    ui,
                    "Grade",
                    status.grade_percent.map(|g| format!("{g:+.1}")),
                    "%",
                );
                let target = match status.control {
                    Control::None => None,
                    _ => status.target_w.map(|w| w.to_string()),
                };
                metric(ui, "Target", target, "W");
                let control = match status.control {
                    Control::None => "–",
                    Control::Simulation => "Road",
                    Control::Target => "ERG",
                };
                metric(ui, "Mode", Some(control.into()), "");
                ui.end_row();
            });
        if status.mode == Mode::ControllerOnly {
            ui.label(RichText::new("In this mode MyWhoosh owns the trainer, so live numbers are shown in MyWhoosh.").color(MUTED));
        }
    });

    card(ui, |ui| {
        ui.label(RichText::new("Connections").strong());
        if status.mode != Mode::ControllerOnly {
            dot(
                ui,
                status.trainer.connected,
                "Trainer",
                status.trainer.name.clone(),
            );
        }
        let battery = status
            .controller
            .battery_percent
            .map(|b| format!("battery {b}%"));
        let detail = match (&status.controller.name, battery) {
            (Some(n), Some(b)) => Some(format!("{n}, {b}")),
            (n, b) => n.clone().or(b),
        };
        dot(ui, status.controller.connected, "Controller", detail);
        let whoosh = status.app_trainer_links > 0 || status.app_controller_links > 0;
        let detail = status.address.map(|a| format!("serving on {a}"));
        dot(ui, whoosh, "MyWhoosh", detail);
    });

    card(ui, |ui| {
        ui.label(RichText::new("In MyWhoosh").strong());
        ui.label(status.mode.description());
        match status.mode {
            Mode::OsmolGears => {
                ui.label("Device Connection > tap LAN > pair “OsmolControl” as the trainer.");
            }
            Mode::VirtualTrainer => {
                ui.label("Device Connection > tap LAN > pair “OsmolControl” as the trainer.");
                ui.label("Then the controller icon (top right) > OsmolControl.");
            }
            Mode::ControllerOnly => {
                ui.label("Device Connection > controller icon (top right) > OsmolControl.");
            }
        }
    });
}
