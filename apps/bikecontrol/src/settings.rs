//! Settings tab: how OsmolControl works with MyWhoosh, rider mass, network and Bluetooth.
use crate::{MUTED, WARN};
use bike_session::{Mode, Settings};
use eframe::egui::{self, RichText};
use std::net::Ipv4Addr;

/// Climb difficulty presets, in percent of the real road grade.
const DIFFICULTY_PRESETS: [(f64, &str); 6] = [
    (25.0, "Recovery"),
    (50.0, "Easy"),
    (75.0, "Moderate"),
    (100.0, "Real road"),
    (125.0, "Hard"),
    (150.0, "Brutal"),
];

/// Name of the nearest preset at or below `percent`.
fn difficulty_label(percent: f64) -> &'static str {
    DIFFICULTY_PRESETS
        .iter()
        .rev()
        .find(|(p, _)| percent >= p - 0.5)
        .map_or(DIFFICULTY_PRESETS[0].1, |(_, label)| label)
}

/// `demo_address` replaces the detected LAN address in demo mode (screenshots).
pub fn show(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    running: bool,
    demo_address: Option<Ipv4Addr>,
) {
    if running {
        ui.label(RichText::new("Changes apply from the next ride.").color(WARN));
    }
    ui.label(RichText::new("Mode").strong());
    for mode in Mode::ALL {
        ui.radio_value(
            &mut settings.mode,
            mode,
            RichText::new(mode.label()).size(15.0),
        );
        ui.indent(mode.label(), |ui| {
            ui.label(RichText::new(mode.description()).color(MUTED));
        });
    }
    ui.separator();

    egui::Grid::new("settings")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Rider + bike");
            ui.add(
                egui::DragValue::new(&mut settings.mass_kg)
                    .range(30.0..=200.0)
                    .speed(0.5)
                    .suffix(" kg"),
            );
            ui.end_row();

            ui.label("Climb difficulty").on_hover_text(
                "How hard MyWhoosh's hills feel with virtual shifting. 100 % rides the road grade \
                 shown in MyWhoosh; MyWhoosh's own halving of the grade is undone.",
            );
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 240.0;
                    ui.add(
                        egui::Slider::new(&mut settings.difficulty_percent, 25.0..=150.0)
                            .smart_aim(false)
                            .fixed_decimals(0)
                            .suffix(" %"),
                    );
                    ui.label(
                        RichText::new(difficulty_label(settings.difficulty_percent)).color(MUTED),
                    );
                });
                for row in DIFFICULTY_PRESETS.chunks(3) {
                    ui.horizontal(|ui| {
                        for &(percent, label) in row {
                            let current = (settings.difficulty_percent - percent).abs() < 0.5;
                            if ui
                                .selectable_label(current, format!("{label} {percent:.0} %"))
                                .clicked()
                            {
                                settings.difficulty_percent = percent;
                            }
                        }
                    });
                }
            });
            ui.end_row();

            ui.label("Network address");
            ui.horizontal(|ui| {
                let mut auto = settings.bind.is_none();
                if ui.checkbox(&mut auto, "Automatic").changed() {
                    settings.bind = if auto {
                        None
                    } else {
                        bike_session::lan_address().ok()
                    };
                }
                if let Some(address) = &mut settings.bind {
                    let mut text = address.to_string();
                    if ui
                        .add(egui::TextEdit::singleline(&mut text).desired_width(130.0))
                        .changed()
                        && let Ok(parsed) = text.parse::<Ipv4Addr>()
                    {
                        *address = parsed;
                    }
                } else if let Some(found) =
                    demo_address.or_else(|| bike_session::lan_address().ok())
                {
                    ui.label(RichText::new(found.to_string()).color(MUTED));
                }
            });
            ui.end_row();

            ui.label("Bluetooth adapter");
            ui.add(egui::DragValue::new(&mut settings.adapter).range(0..=7));
            ui.end_row();
        });
    ui.label(
        RichText::new(
            "Adapter 0 is the system default; change it only with several Bluetooth adapters.",
        )
        .color(MUTED),
    );
    ui.separator();
    if ui.button("Reset to defaults").clicked() {
        *settings = Settings::default();
    }
}
