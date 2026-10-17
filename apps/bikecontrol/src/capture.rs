//! `--screenshot <file>`: save the window's own frame as PNG, for the manual. Works regardless of
//! which window has focus, unlike desktop screenshot tools.
use eframe::egui;
use std::path::{Path, PathBuf};

/// Frames to wait so fonts, layout and demo values have settled.
const SETTLE_FRAMES: u32 = 15;

#[derive(Default)]
pub struct Capture {
    target: Option<PathBuf>,
    frames: u32,
    requested: bool,
}

impl Capture {
    pub fn new(target: Option<PathBuf>) -> Self {
        Self {
            target,
            ..Self::default()
        }
    }

    /// Call once per frame inside the viewport to capture; true once the PNG is written.
    pub fn step(&mut self, ctx: &egui::Context) -> bool {
        let Some(target) = &self.target else {
            return false;
        };
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            if let Err(e) = save(target, &image) {
                tracing::error!(error = %e, "screenshot not saved");
            }
            self.target = None;
            return true;
        }
        self.frames += 1;
        if self.frames >= SETTLE_FRAMES && !self.requested {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
        false
    }
}

fn save(path: &Path, image: &egui::ColorImage) -> anyhow::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let [w, h] = image.size;
    let mut encoder = png::Encoder::new(file, u32::try_from(w)?, u32::try_from(h)?);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
