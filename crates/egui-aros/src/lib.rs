//! Runs an egui program in an Intuition window on AROS.
//!
//! The window, input, repaint timers and the text clipboard come from winit's AROS backend and
//! egui-winit (with the AROS crate patches this repository applies); the frame is drawn by a
//! CPU renderer ([`raster`]) and copied into the window. [`run`] returns when the window has
//! closed: the program decides what happens next (saving, ending its worker threads, exiting).
//!
//! Lifetime contract:
//! - [`run`] owns the event loop and the window. It returns after the window is closed and every
//!   texture is freed.
//! - Repaint requests from other threads ([`egui::Context::request_repaint`]) wake the loop
//!   through a handle that is disabled before the loop ends: a request made after that, or
//!   during the shutdown, does nothing.
//! - [`WindowHandle`] (for requesters, the title, the busy pointer) holds the window weakly;
//!   once the window is gone its calls do nothing and return `None`/`false`.
//! - The library never exits the process and does not stop thread pools.

pub mod raster;

#[cfg(feature = "asl")]
pub mod asl;

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use egui::{ViewportCommand, ViewportEvent, ViewportId, ViewportInfo};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

#[cfg(target_os = "aros")]
mod sys {
    use std::ffi::c_char;
    extern "C" {
        pub fn ea_present(win: *mut std::ffi::c_void, rgba: *const u8, width: i32, height: i32, stride: i32);
        pub fn ea_set_busy(win: *mut std::ffi::c_void, busy: i32);
        #[cfg_attr(not(feature = "asl"), allow(dead_code))]
        pub fn ea_file_request(
            win: *mut std::ffi::c_void,
            save: i32,
            mode: i32,
            title: *const c_char,
            dir: *const c_char,
            file: *const c_char,
            pattern: *const c_char,
            out: *mut c_char,
            outlen: i32,
        ) -> i32;
        pub fn ea_task_sigalloc() -> u32;
    }
}

/// The task's allocated signals (`tc_SigAlloc`), for leak checks; 0 on other systems.
pub fn task_signals() -> u32 {
    #[cfg(target_os = "aros")]
    // SAFETY: reads a field of the calling task.
    return unsafe { sys::ea_task_sigalloc() };
    #[cfg(not(target_os = "aros"))]
    0
}

/// How the window opens and how the loop behaves.
#[derive(Clone, Debug)]
pub struct Options {
    pub title: String,
    pub inner_size: (u32, u32),
    pub min_inner_size: Option<(u32, u32)>,
    /// Background the frame is cleared to before painting.
    pub clear_color: [u8; 3],
    /// Longest sleep when nothing asks for a repaint, so that [`App::logic`] still gets a turn
    /// for background work. `None`: sleep until an event or a repaint request.
    pub idle_poll: Option<Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            title: "egui".to_owned(),
            inner_size: (800, 600),
            min_inner_size: None,
            clear_color: [0, 0, 0],
            idle_poll: None,
        }
    }
}

/// What one frame cost, for programs that measure themselves.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameReport {
    /// Frames shown so far, this one included.
    pub frame: u64,
    pub width: u32,
    pub height: u32,
    /// Input events delivered since the previous frame.
    pub events: u32,
    /// From the oldest and the newest of those events' arrival to the frame being shown.
    pub latency_oldest: Option<Duration>,
    pub latency_newest: Option<Duration>,
    /// The loop slept this long before the frame.
    pub waited: Duration,
    pub logic: Duration,
    pub ui: Duration,
    pub tessellate: Duration,
    pub textures: Duration,
    pub raster: Duration,
    pub present: Duration,
    pub vertices: u32,
    pub texture_pixels: u64,
    /// The next repaint egui asked for, if any (`Duration::ZERO`: at once).
    pub next_repaint: Option<Duration>,
}

/// The program. Every method runs on the thread that called [`run`].
pub trait App {
    /// Called once, before the first frame, with a handle to the window.
    fn started(&mut self, _window: WindowHandle) {}

    /// Called before [`Self::ui`] in every frame; no drawing here.
    fn logic(&mut self, _ctx: &egui::Context) {}

    fn ui(&mut self, ui: &mut egui::Ui);

    /// A last look at the frame's input before egui sees it.
    fn raw_input_hook(&mut self, _raw: &mut egui::RawInput) {}

    /// The close gadget was used. Return `false` to keep the window open (to ask about unsaved
    /// changes first, for instance); close it later with [`ViewportCommand::Close`] or by
    /// returning `true` from [`Self::quit_requested`].
    fn close_requested(&mut self) -> bool {
        true
    }

    /// Checked after every frame: `true` closes the window.
    fn quit_requested(&self) -> bool {
        false
    }

    /// After a frame is shown.
    fn frame_done(&mut self, _report: &FrameReport, _renderer: &raster::Renderer) {}
}

/// Why [`run`] could not run the program.
#[derive(Debug)]
pub enum Error {
    EventLoop(winit::error::EventLoopError),
    Window(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::EventLoop(e) => write!(f, "event loop: {e}"),
            Error::Window(e) => write!(f, "window: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// A handle to the open window for services that need it as a parent (requesters) or change it
/// (title, busy pointer). Holds the window weakly: after it closes, calls do nothing.
#[derive(Clone)]
pub struct WindowHandle(Weak<Window>);

impl WindowHandle {
    /// The Intuition `struct Window *` while the window is open.
    pub fn raw(&self) -> Option<*mut std::ffi::c_void> {
        #[cfg_attr(not(target_os = "aros"), allow(unused_imports))]
        use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
        let window = self.0.upgrade()?;
        match window.window_handle().ok()?.as_raw() {
            #[cfg(target_os = "aros")]
            RawWindowHandle::Aros(h) => Some(h.window.as_ptr()),
            _ => None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.0.strong_count() > 0
    }

    pub fn set_title(&self, title: &str) {
        if let Some(window) = self.0.upgrade() {
            window.set_title(title);
        }
    }

    /// The system busy pointer over the window.
    pub fn set_busy(&self, busy: bool) {
        #[cfg(target_os = "aros")]
        if let Some(win) = self.raw() {
            // SAFETY: `win` is the open window; the call only changes its pointer.
            unsafe { sys::ea_set_busy(win, busy as i32) };
        }
        #[cfg(not(target_os = "aros"))]
        let _ = busy;
    }
}

/// Wakes the loop from other threads. Disabled (under its lock, so no wake is in flight) when
/// the loop ends; later requests are ignored.
struct Waker {
    proxy: Mutex<Option<EventLoopProxy<()>>>,
}

impl Waker {
    fn wake(&self) {
        if let Some(proxy) = self.proxy.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            let _ = proxy.send_event(());
        }
    }

    fn disable(&self) {
        self.proxy.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

/// The earliest repaint egui asked for, from any thread.
struct Repaint {
    due: Mutex<Option<Instant>>,
    waker: Waker,
    ui_thread: std::thread::ThreadId,
}

impl Repaint {
    fn request(&self, at: Instant) {
        let mut due = self.due.lock().unwrap_or_else(PoisonError::into_inner);
        *due = Some(due.map_or(at, |d| d.min(at)));
        drop(due);
        // Only another thread has to wake the loop. A request made on the loop's own thread comes
        // during a frame or an event (the caret's next blink, a tooltip's delay), and the loop
        // reads `due` before it sleeps; a signal as well would end every sleep at once, and a
        // requested delay would never happen.
        if std::thread::current().id() != self.ui_thread {
            self.waker.wake();
        }
    }

    fn take_due(&self, now: Instant) -> bool {
        let mut due = self.due.lock().unwrap_or_else(PoisonError::into_inner);
        match *due {
            Some(t) if t <= now => {
                *due = None;
                true
            }
            _ => false,
        }
    }

    fn next(&self) -> Option<Instant> {
        *self.due.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Running {
    window: Rc<Window>,
    state: egui_winit::State,
    renderer: raster::Renderer,
    info: ViewportInfo,
    close_requested: bool,
    /// Arrival of the first and last input event since the last frame.
    events: Option<(Instant, Instant, u32)>,
}

struct Runner<'a> {
    options: Options,
    ctx: egui::Context,
    app: &'a mut dyn App,
    repaint: Arc<Repaint>,
    running: Option<Running>,
    error: Option<Error>,
    start: Instant,
    frames: u64,
    slept_since: Instant,
    /// A redraw is pending in winit's queue.
    redraw_queued: bool,
}

impl Runner<'_> {
    fn open(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Error> {
        let (w, h) = self.options.inner_size;
        let mut attributes =
            Window::default_attributes().with_title(self.options.title.clone()).with_inner_size(winit::dpi::PhysicalSize::new(w, h));
        if let Some((w, h)) = self.options.min_inner_size {
            attributes = attributes.with_min_inner_size(winit::dpi::PhysicalSize::new(w, h));
        }
        let window = Rc::new(event_loop.create_window(attributes).map_err(|e| Error::Window(e.to_string()))?);
        let size = window.inner_size();
        let state = egui_winit::State::new(self.ctx.clone(), ViewportId::ROOT, event_loop, Some(1.0), None, Some(16384));
        let mut info = ViewportInfo::default();
        egui_winit::update_viewport_info(&mut info, &self.ctx, &window, true);
        self.app.started(WindowHandle(Rc::downgrade(&window)));
        self.running = Some(Running {
            window,
            state,
            renderer: raster::Renderer::new(size.width as usize, size.height as usize),
            info,
            close_requested: false,
            events: None,
        });
        self.queue_redraw();
        Ok(())
    }

    fn queue_redraw(&mut self) {
        if let Some(r) = &self.running {
            if !self.redraw_queued {
                self.redraw_queued = true;
                r.window.request_redraw();
            }
        }
    }

    /// One frame: input, the app, tessellation, the CPU renderer, presentation. Returns whether
    /// the window should close.
    fn frame(&mut self) -> bool {
        let Some(r) = self.running.as_mut() else { return false };
        let began = Instant::now();
        let waited = began.saturating_duration_since(self.slept_since);
        let mut raw = r.state.take_egui_input(&r.window);
        raw.time = Some(self.start.elapsed().as_secs_f64());
        egui_winit::update_viewport_info(&mut r.info, &self.ctx, &r.window, false);
        let close_requested = std::mem::take(&mut r.close_requested);
        let mut info = r.info.clone();
        if close_requested {
            info.events.push(ViewportEvent::Close);
        }
        raw.viewports.insert(ViewportId::ROOT, info);
        self.app.raw_input_hook(&mut raw);
        let events = r.events.take();

        let input_done = Instant::now();
        let mut logic_done = input_done;
        let app = &mut *self.app;
        let out = self.ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx());
            logic_done = Instant::now();
            app.ui(ui);
        });
        let ui_done = Instant::now();

        let r = self.running.as_mut().expect("the window is open during a frame");
        r.state.handle_platform_output(&r.window, out.platform_output);
        let mut closing = close_requested && self.app.close_requested();
        let mut next_repaint = None;
        if let Some(vo) = out.viewport_output.get(&ViewportId::ROOT) {
            let mut commands = Vec::new();
            for cmd in &vo.commands {
                match cmd {
                    ViewportCommand::Close => closing = true,
                    ViewportCommand::CancelClose => closing = false,
                    other => commands.push(other.clone()),
                }
            }
            let mut actions = Vec::new();
            egui_winit::process_viewport_commands(&self.ctx, &mut r.info, commands, &r.window, &mut actions);
            if vo.repaint_delay < Duration::from_secs(3600) {
                next_repaint = Some(vo.repaint_delay);
                self.repaint.request(Instant::now() + vo.repaint_delay);
            }
        }

        let size = r.window.inner_size();
        let (w, h) = (size.width.max(1) as usize, size.height.max(1) as usize);
        if (w, h) != (r.renderer.width, r.renderer.height) {
            r.renderer.resize(w, h);
        }
        let paint_start = Instant::now();
        let primitives = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        let tessellated = Instant::now();
        r.renderer.update_textures(&out.textures_delta);
        let uploaded = Instant::now();
        r.renderer.clear(self.options.clear_color);
        r.renderer.paint(&primitives, out.pixels_per_point);
        r.renderer.free_textures(&out.textures_delta);
        let rasterized = Instant::now();
        present(&r.window, &r.renderer);
        let shown = Instant::now();
        self.frames += 1;

        let report = FrameReport {
            frame: self.frames,
            width: w as u32,
            height: h as u32,
            events: events.map_or(0, |e| e.2),
            latency_oldest: events.map(|e| shown - e.0),
            latency_newest: events.map(|e| shown - e.1),
            waited,
            logic: logic_done - input_done,
            ui: ui_done - logic_done,
            tessellate: tessellated - paint_start,
            textures: uploaded - tessellated,
            raster: rasterized - uploaded,
            present: shown - rasterized,
            vertices: primitives
                .iter()
                .map(|p| match &p.primitive {
                    egui::epaint::Primitive::Mesh(m) => m.vertices.len() as u32,
                    egui::epaint::Primitive::Callback(_) => 0,
                })
                .sum(),
            texture_pixels: out.textures_delta.set.iter().flat_map(|(_, ds)| ds.iter()).map(|d| (d.image.width() * d.image.height()) as u64).sum(),
            next_repaint,
        };
        self.app.frame_done(&report, &r.renderer);
        closing || self.app.quit_requested()
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(mut r) = self.running.take() {
            // Textures belong to the renderer; nothing of the window outlives this.
            r.renderer.free_all();
            drop(r);
        }
        event_loop.exit();
    }
}

fn present(window: &Window, renderer: &raster::Renderer) {
    #[cfg(target_os = "aros")]
    {
        use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
        if let Ok(handle) = window.window_handle() {
            if let RawWindowHandle::Aros(h) = handle.as_raw() {
                // SAFETY: the window is open (we hold it) and the frame is width x height RGBA
                // with the given stride.
                unsafe {
                    sys::ea_present(
                        h.window.as_ptr(),
                        renderer.frame.as_ptr(),
                        renderer.width as i32,
                        renderer.height as i32,
                        renderer.stride() as i32,
                    )
                };
            }
        }
    }
    #[cfg(not(target_os = "aros"))]
    let _ = (window, renderer);
}

impl ApplicationHandler<()> for Runner<'_> {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::WaitCancelled { .. } | StartCause::ResumeTimeReached { .. } | StartCause::Poll = cause {
            if self.repaint.take_due(Instant::now()) {
                self.queue_redraw();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_none() && self.error.is_none() {
            if let Err(e) = self.open(event_loop) {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        if self.repaint.take_due(Instant::now()) {
            self.queue_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if self.running.is_none() {
            return;
        }
        match event {
            WindowEvent::RedrawRequested => {
                self.redraw_queued = false;
                if self.frame() {
                    self.close(event_loop);
                }
            }
            WindowEvent::CloseRequested => {
                if let Some(r) = self.running.as_mut() {
                    r.close_requested = true;
                }
                self.queue_redraw();
            }
            WindowEvent::Destroyed => {}
            event => {
                let now = Instant::now();
                let is_input = matches!(
                    event,
                    WindowEvent::KeyboardInput { .. }
                        | WindowEvent::CursorMoved { .. }
                        | WindowEvent::MouseInput { .. }
                        | WindowEvent::MouseWheel { .. }
                );
                let Some(r) = self.running.as_mut() else { return };
                let response = r.state.on_window_event(&r.window, &event);
                if is_input {
                    r.events = Some(r.events.map_or((now, now, 1), |(first, _, n)| (first, now, n + 1)));
                }
                if response.repaint {
                    self.queue_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.repaint.take_due(now) {
            self.queue_redraw();
        }
        if self.redraw_queued {
            // winit delivers the redraw before it sleeps.
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        let poll = self.options.idle_poll.map(|d| now + d);
        let wake = match (self.repaint.next(), poll) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.slept_since = now;
        event_loop.set_control_flow(wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
        if poll.is_some_and(|p| wake == Some(p)) {
            // The idle poll is a repaint too: it is what gives `logic` its turn.
            self.repaint.request(poll.unwrap_or(now));
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Before winit frees the loop's signal: no wake may reach it afterwards.
        self.repaint.waker.disable();
        if let Some(mut r) = self.running.take() {
            r.renderer.free_all();
        }
    }
}

/// Opens the window, runs the program until the window closes, and returns. `ctx` may be shared
/// with the program's threads; their repaint requests wake the loop while it runs.
pub fn run(options: Options, ctx: egui::Context, app: &mut dyn App) -> Result<(), Error> {
    let event_loop = EventLoop::<()>::with_user_event().build().map_err(Error::EventLoop)?;
    let repaint = Arc::new(Repaint {
        due: Mutex::new(None),
        waker: Waker { proxy: Mutex::new(Some(event_loop.create_proxy())) },
        ui_thread: std::thread::current().id(),
    });
    {
        let repaint = repaint.clone();
        ctx.set_request_repaint_callback(move |info| repaint.request(Instant::now() + info.delay));
    }
    let now = Instant::now();
    let mut runner = Runner {
        options,
        ctx: ctx.clone(),
        app,
        repaint: repaint.clone(),
        running: None,
        error: None,
        start: now,
        frames: 0,
        slept_since: now,
        redraw_queued: false,
    };
    let result = event_loop.run_app(&mut runner).map_err(Error::EventLoop);
    // Also covers a loop that ended without `exiting` (an error before the window opened).
    repaint.waker.disable();
    // The callback holds the repaint state; requests after this point go nowhere.
    ctx.set_request_repaint_callback(|_| {});
    let error = runner.error.take();
    drop(runner);
    result?;
    error.map_or(Ok(()), Err)
}

/// Shared single-threaded state, for programs that keep their [`WindowHandle`] in services.
pub type SharedHandle = Rc<RefCell<Option<WindowHandle>>>;
