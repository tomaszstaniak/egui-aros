//! The scene and the measurement shared by the probes, so that every renderer draws the same
//! frame: text, edit fields, a scroll area and a texture with transparency.
//!
//! Environment (all optional):
//! - `EGUI_PROBE_SIZE=<w>x<h>`: the window's inner size (default 720x480).
//! - `EGUI_PROBE_FRAMES=<n>`: close the window after n frames, repainting continuously until
//!   then, so the frame intervals are what a frame costs.
//! - `EGUI_PROBE_REPORT=<file>`: write the measurements there when the window closes.

use std::fmt::Write as _;
use std::time::{Duration, Instant};


pub struct Probe {
    /// Lines shown at the top (the renderer and its version).
    pub header: Vec<(String, String)>,
    name: String,
    notes: String,
    texture: Option<egui::TextureHandle>,
    started: Instant,
    pub frames: u64,
    intervals: Vec<Duration>,
    work: Vec<Duration>,
    last_frame: Option<Instant>,
    close_after: Option<u64>,
    report: Option<String>,
}

pub fn size() -> (u32, u32) {
    std::env::var("EGUI_PROBE_SIZE")
        .ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((720, 480))
}

/// A checkerboard under a radial alpha ramp: transparent edges show whether blending against
/// the panel behind it works.
fn texture_image() -> egui::ColorImage {
    let size = 128;
    let mut pixels = Vec::with_capacity(size * size);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - size as f32 / 2.0;
            let dy = y as f32 - size as f32 / 2.0;
            let alpha = (1.0 - (dx * dx + dy * dy).sqrt() / (size as f32 / 2.0)).clamp(0.0, 1.0);
            let (r, g, b) = if (x / 16 + y / 16) % 2 == 0 { (230, 80, 40) } else { (40, 90, 220) };
            pixels.push(egui::Color32::from_rgba_unmultiplied(r, g, b, (alpha * 255.0) as u8));
        }
    }
    egui::ColorImage::new([size, size], pixels)
}

impl Probe {
    pub fn new(header: Vec<(String, String)>) -> Self {
        Self {
            header,
            name: "AROS".to_owned(),
            notes: "Several lines of text.\nTyping, selection and the clipboard work here.".to_owned(),
            texture: None,
            started: Instant::now(),
            frames: 0,
            intervals: Vec::new(),
            work: Vec::new(),
            last_frame: None,
            close_after: std::env::var("EGUI_PROBE_FRAMES").ok().and_then(|v| v.parse().ok()),
            report: std::env::var("EGUI_PROBE_REPORT").ok(),
        }
    }

    /// The time the previous frame took to produce, where the renderer can tell.
    pub fn record_work(&mut self, work: Duration) {
        self.work.push(work);
    }

    /// Draws the scene. Returns `true` when the frame limit is reached and the window should close.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            self.intervals.push(now - last);
        }
        self.frames += 1;
        let ctx = ui.ctx().clone();
        let texture = self
            .texture
            .get_or_insert_with(|| ctx.load_texture("probe-texture", texture_image(), egui::TextureOptions::LINEAR))
            .clone();

        egui::Panel::top("header").show(ui, |ui| {
            for (name, value) in &self.header {
                ui.label(format!("{name}: {value}"));
            }
        });
        egui::Panel::left("texture").show(ui, |ui| {
            ui.heading("Texture");
            ui.image((texture.id(), texture.size_vec2()));
            ui.label(format!("window {:.0}x{:.0}", ctx.content_rect().width(), ctx.content_rect().height()));
            ui.label(format!("frames {}", self.frames));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut self.name);
            });
            ui.label(format!("Hello, {}! Zażółć gęślą jaźń.", self.name));
            ui.text_edit_multiline(&mut self.notes);
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for i in 0..200 {
                    ui.label(format!("Line {i}: the quick brown fox jumps over the lazy dog."));
                }
            });
        });

        match self.close_after {
            Some(n) if self.frames >= n => true,
            Some(_) => {
                ctx.request_repaint();
                false
            }
            None => false,
        }
    }

    /// Writes the report, with `extra` lines (`name: value`) after the header.
    pub fn write_report(&self, extra: &[(String, String)]) {
        let Some(path) = &self.report else { return };
        let mut out = String::new();
        for (name, value) in self.header.iter().chain(extra) {
            let _ = writeln!(out, "{name}: {value}");
        }
        let stats = |v: &[Duration]| {
            let mut t: Vec<f64> = v.iter().map(Duration::as_secs_f64).collect();
            t.sort_by(f64::total_cmp);
            let pick = |p: f64| t.get(((t.len().saturating_sub(1)) as f64 * p).round() as usize).copied().unwrap_or(0.0) * 1e3;
            format!("n {} median {:.1} p95 {:.1} max {:.1}", t.len(), pick(0.5), pick(0.95), pick(1.0))
        };
        let _ = writeln!(out, "frames: {}", self.frames);
        let _ = writeln!(out, "run_seconds: {:.3}", self.started.elapsed().as_secs_f64());
        let _ = writeln!(out, "frame_interval_ms {}", stats(&self.intervals));
        let _ = writeln!(out, "frame_work_ms {}", stats(&self.work));
        if let Err(error) = std::fs::write(path, out) {
            eprintln!("report {path}: {error}");
        }
    }
}
