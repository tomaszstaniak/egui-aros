//! Logs the task's allocated signal mask (tc_SigAlloc) after each step of a
//! winit + glutin (GLA) window's life, to find which step leaves a signal
//! allocated when the program ends. Steps are cumulative; the first argument
//! picks the last one: `loop`, `window`, `context`, `swap` (default).

use std::num::NonZeroU32;

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext as _, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay as _, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, WindowSurface};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle as _;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

extern "C" {
    fn ea_sigalloc() -> u32;
}

fn log(step: &str) {
    println!("{step:<28} tc_SigAlloc {:08x}", unsafe { ea_sigalloc() });
}

#[derive(PartialEq, PartialOrd, Clone, Copy)]
enum Last {
    Loop,
    Window,
    Context,
    Swap,
}

struct App {
    last: Last,
    window: Option<Window>,
    gl: Option<(PossiblyCurrentContext, Surface<WindowSurface>)>,
    frames: u32,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.last == Last::Loop {
            event_loop.exit();
            return;
        }
        let attributes = WindowAttributes::default().with_title("lifecycle probe").with_inner_size(winit::dpi::LogicalSize::new(320.0, 200.0));
        if self.last == Last::Window {
            self.window = Some(event_loop.create_window(attributes).expect("window"));
            log("window open");
            event_loop.exit();
            return;
        }
        let (window, config) = DisplayBuilder::new()
            .with_window_attributes(Some(attributes))
            .build(event_loop, ConfigTemplateBuilder::new(), |mut configs| configs.next().expect("a GLA config"))
            .expect("display");
        let window = window.expect("window");
        log("window open (glutin-winit)");
        let display = config.display();
        let handle = window.window_handle().expect("handle").as_raw();
        let context = unsafe { display.create_context(&config, &ContextAttributesBuilder::new().build(Some(handle))) }.expect("context");
        log("context created");
        let size = window.inner_size();
        let surface = unsafe {
            display.create_window_surface(
                &config,
                &SurfaceAttributesBuilder::<WindowSurface>::new().build(handle, NonZeroU32::new(size.width.max(1)).unwrap(), NonZeroU32::new(size.height.max(1)).unwrap()),
            )
        }
        .expect("surface");
        let context = context.make_current(&surface).expect("make current");
        log("context current");
        self.window = Some(window);
        self.gl = Some((context, surface));
        if self.last == Last::Context {
            event_loop.exit();
        } else {
            self.window.as_ref().unwrap().request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let WindowEvent::RedrawRequested = event {
            if let Some((context, surface)) = &self.gl {
                surface.swap_buffers(context).expect("swap");
                self.frames += 1;
                if self.frames == 1 {
                    log("first swap");
                }
                if self.frames >= 3 {
                    event_loop.exit();
                } else {
                    self.window.as_ref().unwrap().request_redraw();
                }
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        log("loop exiting");
        self.gl = None;
        log("context dropped");
        self.window = None;
        log("window dropped");
    }
}

fn main() {
    let last = match std::env::args().nth(1).as_deref() {
        Some("loop") => Last::Loop,
        Some("window") => Last::Window,
        Some("context") => Last::Context,
        _ => Last::Swap,
    };
    log("start");
    let event_loop = EventLoop::new().expect("event loop");
    log("event loop created");
    let mut app = App { last, window: None, gl: None, frames: 0 };
    event_loop.run_app(&mut app).expect("run");
    log("run_app returned");
    drop(app);
    log("end of main");
}
