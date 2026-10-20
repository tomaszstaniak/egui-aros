//! The probe scene (`../common/probe.rs`) on eframe's glow renderer: does the standard egui
//! desktop stack (eframe, egui-winit, glutin, winit) run on this system, on which OpenGL, and
//! at what cost per frame? Same environment variables as the CPU probe.

#[path = "../../common/probe.rs"]
mod probe;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::glow::{self, HasContext as _};

fn gl_strings(gl: &glow::Context) -> Vec<(String, String)> {
    let get = |name| unsafe { gl.get_parameter_string(name) };
    vec![
        ("GL_VENDOR".to_owned(), get(glow::VENDOR)),
        ("GL_RENDERER".to_owned(), get(glow::RENDERER)),
        ("GL_VERSION".to_owned(), get(glow::VERSION)),
        ("GL_SHADING_LANGUAGE_VERSION".to_owned(), get(glow::SHADING_LANGUAGE_VERSION)),
    ]
}

fn signals() -> u32 {
    egui_aros::task_signals()
}

/// The probe outlives eframe's app box, so the report can include what happens after
/// `run_native` returns.
struct App(Arc<Mutex<probe::Probe>>);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let mut probe = self.0.lock().unwrap();
        // eframe's time for the previous frame (app code, tessellation and painting).
        if let Some(cpu) = frame.info().cpu_usage {
            probe.record_work(Duration::from_secs_f32(cpu));
        }
        if probe.ui(ui) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn main() {
    let at_start = signals();
    let probe = Arc::new(Mutex::new(probe::Probe::new(Vec::new())));
    let (w, h) = probe::size();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([w as f32, h as f32]).with_min_inner_size([320.0, 240.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let shared = probe.clone();
    let result = eframe::run_native(
        "egui-aros glow probe",
        options,
        Box::new(move |cc| {
            let header = cc.gl.as_deref().map(gl_strings).unwrap_or_default();
            for (name, value) in &header {
                println!("{name}: {value}");
            }
            shared.lock().unwrap().header = header;
            Ok(Box::new(App(shared)))
        }),
    );
    let at_end = signals();
    probe.lock().unwrap().write_report(&[
        ("signals_at_start".to_owned(), format!("{at_start:08x}")),
        ("signals_at_end".to_owned(), format!("{at_end:08x}")),
        ("run_result".to_owned(), format!("{result:?}")),
    ]);
}
