//! The probe scene (`../common/probe.rs`) on egui-aros: winit and egui-winit for the window and
//! input, the CPU renderer for the pixels. Same environment variables as the glow probe, and
//! `EGUI_PROBE_WAKER=1`: another thread requests a repaint every millisecond while the window
//! is open, while it closes, and for half a second after `run` has returned (late wake-ups must
//! do nothing).

#[path = "../../common/probe.rs"]
mod probe;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
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
    let ctx = egui::Context::default();
    let stop = Arc::new(AtomicBool::new(false));
    let wakes = Arc::new(AtomicU64::new(0));
    let waker = std::env::var_os("EGUI_PROBE_WAKER").is_some().then(|| {
        let (ctx, stop, wakes) = (ctx.clone(), stop.clone(), wakes.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                ctx.request_repaint();
                wakes.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    });
    let result = egui_aros::run(options, ctx, &mut app);
    let wakes_at_return = wakes.load(Ordering::SeqCst);
    if let Some(waker) = waker {
        std::thread::sleep(Duration::from_millis(500));
        stop.store(true, Ordering::SeqCst);
        let _ = waker.join();
    }
    let signals_at_end = egui_aros::task_signals();
    app.probe.write_report(&[
        ("signals_at_start".to_owned(), format!("{:08x}", app.signals_at_start)),
        ("signals_at_end".to_owned(), format!("{signals_at_end:08x}")),
        ("run_result".to_owned(), format!("{result:?}")),
        ("wakes_requested".to_owned(), format!("{} ({} after run returned)", wakes.load(Ordering::SeqCst), wakes.load(Ordering::SeqCst) - wakes_at_return)),
    ]);
    if let Err(e) = result {
        eprintln!("egui-aros CPU probe: {e}");
        std::process::exit(1);
    }
}
