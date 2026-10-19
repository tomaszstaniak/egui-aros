//! A small eframe program on the glow renderer: text, edit fields, a scroll
//! area, a texture with transparency and a resizable window.
//!
//! It answers one question: does the standard egui desktop stack (eframe,
//! egui-winit, glutin, winit) run on this system, and on which OpenGL?
//!
//! Environment (all optional):
//! - `EGUI_PROBE_REPORT=<file>`: write the GL strings and frame times there,
//!   at the end of the run.
//! - `EGUI_PROBE_SIZE=<w>x<h>`: the window's inner size (default 720x480).
//! - `EGUI_PROBE_FRAMES=<n>`: close the window after n frames, repainting
//!   continuously until then, so the frame intervals are what a frame costs.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::glow::{self, HasContext as _};

struct Probe {
    gl_info: Vec<(&'static str, String)>,
    name: String,
    notes: String,
    texture: egui::TextureHandle,
    started: Instant,
    frames: u64,
    frame_times: Vec<Duration>,
    cpu_times: Vec<f32>,
    last_frame: Option<Instant>,
    close_after: Option<u64>,
    report: Option<String>,
}

fn gl_strings(gl: &glow::Context) -> Vec<(&'static str, String)> {
    let get = |name| unsafe { gl.get_parameter_string(name) };
    vec![
        ("GL_VENDOR", get(glow::VENDOR)),
        ("GL_RENDERER", get(glow::RENDERER)),
        ("GL_VERSION", get(glow::VERSION)),
        ("GL_SHADING_LANGUAGE_VERSION", get(glow::SHADING_LANGUAGE_VERSION)),
    ]
}

/// A checkerboard under a radial alpha ramp: transparent edges show whether
/// blending against the panel behind it works.
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
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let gl_info = cc.gl.as_deref().map(gl_strings).unwrap_or_default();
        for (name, value) in &gl_info {
            println!("{name}: {value}");
        }
        let texture = cc.egui_ctx.load_texture("probe-texture", texture_image(), egui::TextureOptions::LINEAR);
        Self {
            gl_info,
            name: "AROS".to_owned(),
            notes: "Several lines of text.\nTyping, selection and the clipboard work here.".to_owned(),
            texture,
            started: Instant::now(),
            frames: 0,
            frame_times: Vec::new(),
            cpu_times: Vec::new(),
            last_frame: None,
            close_after: std::env::var("EGUI_PROBE_FRAMES").ok().and_then(|v| v.parse().ok()),
            report: std::env::var("EGUI_PROBE_REPORT").ok(),
        }
    }

    fn write_report(&self) {
        let Some(path) = &self.report else { return };
        let mut out = String::new();
        for (name, value) in &self.gl_info {
            let _ = writeln!(out, "{name}: {value}");
        }
        let mut times: Vec<_> = self.frame_times.iter().map(Duration::as_secs_f64).collect();
        times.sort_by(f64::total_cmp);
        let pick = |p: f64| times.get(((times.len().saturating_sub(1)) as f64 * p).round() as usize).copied().unwrap_or(0.0);
        let mut cpu = self.cpu_times.clone();
        cpu.sort_by(f32::total_cmp);
        let cpu_median = cpu.get(cpu.len() / 2).copied().unwrap_or(0.0);
        let _ = writeln!(out, "frames: {}", self.frames);
        let _ = writeln!(out, "app_cpu_ms median {:.1}", cpu_median * 1e3);
        let _ = writeln!(out, "run_seconds: {:.3}", self.started.elapsed().as_secs_f64());
        let _ = writeln!(out, "frame_interval_ms median {:.1} p95 {:.1} max {:.1}", pick(0.5) * 1e3, pick(0.95) * 1e3, pick(1.0) * 1e3);
        if let Err(error) = std::fs::write(path, out) {
            eprintln!("report {path}: {error}");
        }
    }
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            self.frame_times.push(now - last);
        }
        self.frames += 1;
        // eframe's time in the previous frame's app code (update, tessellation excluded).
        if let Some(cpu) = frame.info().cpu_usage {
            self.cpu_times.push(cpu);
        }

        egui::Panel::top("gl").show(ui, |ui| {
            for (name, value) in &self.gl_info {
                ui.label(format!("{name}: {value}"));
            }
            if self.gl_info.is_empty() {
                ui.label("No glow context.");
            }
        });
        egui::Panel::left("texture").show(ui, |ui| {
            ui.heading("Texture");
            ui.image((self.texture.id(), self.texture.size_vec2()));
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
            Some(n) if self.frames >= n => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Some(_) => ctx.request_repaint(),
            None => {}
        }
    }

    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        self.write_report();
    }
}

fn size() -> [f32; 2] {
    std::env::var("EGUI_PROBE_SIZE")
        .ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some([w.parse().ok()?, h.parse().ok()?])))
        .unwrap_or([720.0, 480.0])
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(size()).with_min_inner_size([320.0, 240.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native("egui-aros glow probe", options, Box::new(|cc| Ok(Box::new(Probe::new(cc)))))
}
