//! Compact always-on-top window for riding: gear, power, cadence, grade. Drag it anywhere.
use crate::backend::Backend;
use crate::{ACCENT, MUTED};
use bike_session::{Mode, Status};
use eframe::egui::{self, RichText};

pub const SIZE: [f32; 2] = [300.0, 150.0];

/// Returns false when the user closed the overlay.
pub fn show(ctx: &egui::Context, status: &Status, backend: &Backend) -> bool {
    let mut open = true;
    let builder = egui::ViewportBuilder::default()
        .with_title("OsmolControl overlay")
        .with_inner_size(SIZE)
        .with_always_on_top()
        .with_decorations(false)
        .with_resizable(false);
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("overlay"),
        builder,
        |ctx, _class| {
            if ctx.input(|i| i.viewport().close_requested()) {
                open = false;
            }
            panel(ctx, status, backend, &mut open);
        },
    );
    open
}

/// The overlay's contents, drawn into whichever viewport `ctx` belongs to.
pub fn panel(ctx: &egui::Context, status: &Status, backend: &Backend, open: &mut bool) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::default()
                .fill(egui::Color32::from_rgb(0x14, 0x16, 0x1a))
                .inner_margin(10),
        )
        .show(ctx, |ui| {
            let drag = ui.interact(
                ui.max_rect(),
                egui::Id::new("overlay-drag"),
                egui::Sense::click_and_drag(),
            );
            if drag.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            ui.horizontal(|ui| {
                match (status.mode, status.gear) {
                    (Mode::OsmolGears, Some(g)) => {
                        ui.label(
                            RichText::new(g.number.to_string())
                                .size(46.0)
                                .strong()
                                .color(ACCENT),
                        );
                        ui.vertical(|ui| {
                            ui.label(RichText::new(format!("/ {}", g.of)).color(MUTED));
                            ui.label(
                                RichText::new(format!("{}×{}", g.chainring, g.cog))
                                    .size(18.0)
                                    .strong(),
                            );
                        });
                    }
                    _ => {
                        ui.label(RichText::new("OsmolControl").size(18.0).strong());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    if ui
                        .small_button("✕")
                        .on_hover_text("Close overlay")
                        .clicked()
                    {
                        *open = false;
                    }
                    if ui.small_button("+").on_hover_text("Harder").clicked() {
                        backend.shift(true);
                    }
                    if ui.small_button("−").on_hover_text("Easier").clicked() {
                        backend.shift(false);
                    }
                });
            });
            let fresh = status.telemetry_fresh();
            let text = |v: Option<String>| v.unwrap_or_else(|| "–".into());
            ui.horizontal(|ui| {
                let power = text(status.power_w.filter(|_| fresh).map(|w| w.to_string()));
                let cadence = text(
                    status
                        .cadence_rpm
                        .filter(|_| fresh)
                        .map(|c| format!("{c:.0}")),
                );
                let grade = text(status.grade_percent.map(|g| format!("{g:+.1}")));
                ui.label(RichText::new(format!("{power} W")).size(22.0).strong());
                ui.label(RichText::new(format!("{cadence} rpm")).size(22.0).strong());
                ui.label(RichText::new(format!("{grade} %")).size(22.0).color(MUTED));
            });
        });
}
