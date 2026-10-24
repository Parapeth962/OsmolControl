//! OsmolControl desktop app: the same riding session as `bikectl start`, with a window,
//! gear templates, settings and an always-on-top overlay. All protocol work lives in the crates.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod backend;
mod capture;
mod controllers;
mod gears;
mod overlay;
mod ride;
mod settings;

use backend::Backend;
use bike_session::{Phase, Settings, Status};
use eframe::egui::{self, Color32, RichText};

const SETTINGS_KEY: &str = "settings";
const OVERLAY_KEY: &str = "overlay";

pub const ACCENT: Color32 = Color32::from_rgb(0x2e, 0xc4, 0x8f);
pub const WARN: Color32 = Color32::from_rgb(0xf2, 0xb1, 0x34);
pub const MUTED: Color32 = Color32::from_gray(150);
/// Matches the installed `osmolcontrol.desktop`, so GNOME on Wayland shows the right icon.
const APP_ID: &str = "osmolcontrol";

/// Window, taskbar and dock icon; the window falls back to the platform default if it fails.
pub fn icon() -> Option<std::sync::Arc<egui::IconData>> {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/osmolcontrol-256.png"))
        .map(std::sync::Arc::new)
        .ok()
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    // `--demo` shows sample data; `--tab <name>` and `--overlay` help with screenshots.
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    let force_overlay = args.iter().any(|a| a == "--overlay");
    // `--screenshot <file>` saves the main window (with `--overlay`: the overlay) and exits.
    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);
    // Secondary windows cannot be captured, so the overlay is shot in the main window.
    let overlay_preview = force_overlay && screenshot.is_some();
    let tab = match args
        .iter()
        .position(|a| a == "--tab")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
    {
        Some("gears") => Tab::Gears,
        Some("controllers") => Tab::Controllers,
        Some("settings") => Tab::Settings,
        Some("log") => Tab::Log,
        _ => Tab::Ride,
    };
    let viewport = if overlay_preview {
        egui::ViewportBuilder::default()
            .with_inner_size(overlay::SIZE)
            .with_decorations(false)
    } else {
        egui::ViewportBuilder::default()
            .with_inner_size([560.0, 720.0])
            .with_min_inner_size([420.0, 520.0])
    };
    let mut viewport = viewport.with_title(bike_core::APP_NAME).with_app_id(APP_ID);
    if let Some(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        bike_core::APP_NAME,
        options,
        Box::new(move |cc| {
            apply_theme(&cc.egui_ctx);
            let backend = Backend::new(cc.egui_ctx.clone())?;
            if demo {
                backend.show_demo();
            }
            let settings = cc
                .storage
                .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
                .unwrap_or_default();
            let overlay = cc
                .storage
                .and_then(|s| eframe::get_value(s, OVERLAY_KEY))
                .unwrap_or(false)
                || force_overlay;
            Ok(Box::new(App {
                backend,
                settings,
                tab,
                overlay,
                gears: gears::GearsTab::default(),
                controllers: controllers::ControllersTab::default(),
                demo,
                capture: capture::Capture::new(screenshot),
                overlay_preview,
            }))
        }),
    )
}

fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.selection.bg_fill = ACCENT.linear_multiply(0.6);
    visuals.hyperlink_color = ACCENT;
    ctx.set_visuals(visuals);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Ride,
    Gears,
    Controllers,
    Settings,
    Log,
}

struct App {
    backend: Backend,
    settings: Settings,
    tab: Tab,
    overlay: bool,
    gears: gears::GearsTab,
    controllers: controllers::ControllersTab,
    demo: bool,
    capture: capture::Capture,
    overlay_preview: bool,
}

pub fn phase_text(status: &Status, running: bool) -> (&'static str, Color32) {
    match status.phase {
        _ if status.error.is_some() && !running => ("Stopped with an error", WARN),
        Phase::Idle => ("Not started", MUTED),
        Phase::SearchingTrainer => ("Looking for the trainer...", WARN),
        Phase::SearchingController => ("Looking for the controller...", WARN),
        Phase::Running => ("Running", ACCENT),
        Phase::Stopped => ("Stopped", MUTED),
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, status: &Status) {
        // The demo shows a ride in progress.
        let running = self.backend.running() || self.demo;
        ui.horizontal(|ui| {
            ui.heading(RichText::new(bike_core::APP_NAME).strong());
            if self.demo {
                ui.label(RichText::new("DEMO DATA").color(WARN).strong());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if ui.button(RichText::new("■ Stop").size(16.0)).clicked() {
                        self.backend.stop();
                    }
                } else if ui
                    .add(
                        egui::Button::new(RichText::new("▶ Start ride").size(16.0).strong())
                            .fill(ACCENT.linear_multiply(0.5)),
                    )
                    .clicked()
                {
                    self.backend.start(self.settings.clone());
                    self.tab = Tab::Ride;
                }
                ui.toggle_value(&mut self.overlay, "Overlay")
                    .on_hover_text("Small always-on-top window with gear, power and cadence");
                let (text, color) = phase_text(status, running);
                ui.label(RichText::new(text).color(color));
            });
        });
        ui.horizontal(|ui| {
            for (tab, label) in [
                (Tab::Ride, "Ride"),
                (Tab::Gears, "Gears"),
                (Tab::Controllers, "Controllers"),
                (Tab::Settings, "Settings"),
                (Tab::Log, "Log"),
            ] {
                ui.selectable_value(&mut self.tab, tab, RichText::new(label).size(15.0));
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let status = self.backend.status();
        if self.overlay_preview {
            overlay::panel(ctx, &status, &self.backend, &mut true);
            if self.capture.step(ctx) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        let running = self.backend.running();
        // Keyboard shifting while the window has focus.
        let (up, down) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::Plus)
                    || i.key_pressed(egui::Key::Equals),
                i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::Minus),
            )
        });
        if running && (up || down) && !ctx.wants_keyboard_input() {
            self.backend.shift(up);
        }
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui, &status);
            ui.add_space(2.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.tab {
                    Tab::Ride => ride::show(ui, &status, &self.backend, running),
                    Tab::Gears => self.gears.show(ui, &mut self.settings, running),
                    Tab::Controllers => {
                        self.controllers
                            .show(ui, &mut self.settings, &self.backend, running)
                    }
                    Tab::Settings => settings::show(
                        ui,
                        &mut self.settings,
                        running,
                        status.address.filter(|_| self.demo),
                    ),
                    Tab::Log => {
                        for line in &status.log {
                            ui.label(RichText::new(line).monospace());
                        }
                    }
                });
        });
        if self.overlay {
            self.overlay = overlay::show(ctx, &status, &self.backend);
        }
        if self.capture.step(ctx) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Telemetry goes stale without new data; refresh the display regularly.
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
        eframe::set_value(storage, OVERLAY_KEY, &self.overlay);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.backend.shutdown();
    }
}
