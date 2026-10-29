//! Compact always-on-top window for riding: gear, power, cadence, grade, heart rate. Drag it
//! anywhere.
use crate::backend::Backend;
use crate::{ACCENT, MUTED};
use bike_session::{Mode, Status};
use eframe::egui::{self, RichText};

pub const SIZE: [f32; 2] = [300.0, 150.0];
const TITLE: &str = "OsmolControl overlay";

/// Returns false when the user closed the overlay.
pub fn show(ctx: &egui::Context, status: &Status, backend: &Backend) -> bool {
    let mut open = true;
    // On X11 the window starts hidden and is shown once it is an on-screen display (window
    // types only count when a window is mapped). (last pass drawn, shown)
    let id = egui::Id::new("overlay-window");
    let pass = ctx.cumulative_pass_nr();
    let (last, shown) = ctx
        .data(|d| d.get_temp::<(u64, bool)>(id))
        .unwrap_or_default();
    // Not drawn on the previous pass: the overlay was closed, this is a new window.
    let mut shown = (shown && last + 1 >= pass) || !lift();
    let mut builder = egui::ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size(SIZE)
        .with_always_on_top()
        .with_decorations(false)
        .with_resizable(false)
        .with_visible(shown);
    if let Some(icon) = crate::icon() {
        builder = builder.with_icon(icon);
    }
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("overlay"),
        builder,
        |ctx, _class| {
            if ctx.input(|i| i.viewport().close_requested()) {
                open = false;
            }
            if !shown {
                #[cfg(target_os = "linux")]
                if let Err(e) = crate::x11::on_screen_display(TITLE) {
                    tracing::warn!("overlay may hide behind fullscreen games: {e:#}");
                }
                shown = true;
            }
            panel(ctx, status, backend, &mut open);
        },
    );
    ctx.data_mut(|d| d.insert_temp(id, (pass, shown)));
    open
}

/// Window managers do not move on-screen displays, so the overlay follows the pointer itself:
/// the window keeps its offset to the pointer from the moment it was grabbed. Screen positions
/// come from X11, as window-relative ones shift while the window moves.
#[cfg(target_os = "linux")]
fn drag_by_hand(ctx: &egui::Context, drag: &egui::Response) {
    let id = egui::Id::new("overlay-grab");
    let screen = || {
        let (x, y) = crate::x11::pointer()?;
        Some(egui::pos2(f32::from(x), f32::from(y)) / ctx.pixels_per_point())
    };
    // Grabbed where the button went down; the window has not moved yet at that point.
    if drag.drag_started()
        && let (Some(press), Some(inner), Some(outer)) = ctx.input(|i| {
            let v = i.viewport();
            (i.pointer.press_origin(), v.inner_rect, v.outer_rect)
        })
    {
        ctx.data_mut(|d| d.insert_temp(id, press.to_vec2() + (inner.min - outer.min)));
    }
    if !drag.dragged() {
        ctx.data_mut(|d| d.remove::<egui::Vec2>(id));
        return;
    }
    if let (Some(grab), Some(pointer)) = (ctx.data(|d| d.get_temp::<egui::Vec2>(id)), screen()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pointer - grab));
        // Keep following while the button is held, even between pointer events.
        ctx.request_repaint();
    }
}

/// Whether the overlay is raised above fullscreen games by hand (X11 on Linux).
fn lift() -> bool {
    #[cfg(target_os = "linux")]
    return crate::x11::active();
    #[cfg(not(target_os = "linux"))]
    false
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
            if lift() {
                #[cfg(target_os = "linux")]
                drag_by_hand(ctx, &drag);
            } else if drag.drag_started() {
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
                    if status.heart_rate.name.is_some() {
                        let bpm = status
                            .heart_rate_bpm
                            .filter(|_| status.heart_rate_fresh())
                            .map_or_else(|| "–".into(), |b| b.to_string());
                        ui.add_space(6.0);
                        ui.label(RichText::new(format!("♥ {bpm}")).size(18.0).strong());
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
