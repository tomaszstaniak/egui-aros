//! The probe scene (`../common/probe.rs`) on egui-aros: winit and egui-winit for the window and
//! input, the CPU renderer for the pixels. Same environment variables as the glow probe.

#[path = "../../common/probe.rs"]
mod probe;

use std::time::Duration;

struct App {
    probe: probe::Probe,
    close: bool,
    signals_at_start: u32,
}

impl egui_aros::App for App {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.close |= self.probe.ui(ui);
    }

    fn quit_requested(&self) -> bool {
        self.close
    }

    fn frame_done(&mut self, report: &egui_aros::FrameReport, _renderer: &egui_aros::raster::Renderer) {
        let work: Duration = report.logic + report.ui + report.tessellate + report.textures + report.raster + report.present;
        self.probe.record_work(work);
    }
}

fn main() {
    let signals_at_start = egui_aros::task_signals();
    let header = vec![("renderer".to_owned(), "egui-aros CPU (winit, egui-winit)".to_owned())];
    let mut app = App { probe: probe::Probe::new(header), close: false, signals_at_start };
    let (w, h) = probe::size();
    let options = egui_aros::Options {
        title: "egui-aros CPU probe".to_owned(),
        inner_size: (w, h),
        min_inner_size: Some((320, 240)),
        clear_color: [0x1b, 0x1b, 0x1b],
        idle_poll: None,
    };
    let result = egui_aros::run(options, egui::Context::default(), &mut app);
    let signals_at_end = egui_aros::task_signals();
    app.probe.write_report(&[
        ("signals_at_start".to_owned(), format!("{:08x}", app.signals_at_start)),
        ("signals_at_end".to_owned(), format!("{signals_at_end:08x}")),
        ("run_result".to_owned(), format!("{result:?}")),
    ]);
    if let Err(e) = result {
        eprintln!("egui-aros CPU probe: {e}");
        std::process::exit(1);
    }
}
