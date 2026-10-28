# Using egui-aros in a program

This branch builds on egui 0.37.0 as the Craft programs use it (the storytold
forks of egui and winit, pinned in `upstreams.json`).

```rust
struct MyApp { /* ... */ }

impl egui_aros::App for MyApp {
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Hello, AROS");
    }
}

fn main() {
    let options = egui_aros::Options { title: "My program".into(), inner_size: (800, 600), ..Default::default() };
    let mut app = MyApp { /* ... */ };
    if let Err(e) = egui_aros::run(options, egui::Context::default(), &mut app) {
        eprintln!("{e}");
    }
    // The window is closed here. Save, stop your threads, then return from main.
}
```

## What the library does

- Opens one Intuition window on the Workbench screen and runs the loop: winit's
  AROS backend for the window and its messages, egui-winit for keyboard, text,
  pointer, wheel, focus and resizing.
- Repaints when egui asks: at once, after a delay (the caret's blink), or when
  another thread calls `Context::request_repaint`. Repaint requests made on the
  loop's own thread do not wake it; it reads them before it sleeps.
- Draws every frame with a CPU renderer (textured triangles, premultiplied
  alpha, egui's anti-aliasing) and copies it into the window with
  `WritePixelArray`, or with OpenGL (see "Renderer" below). Partial texture
  updates (`ImageDelta` with a position) are applied in order.
- Draws every egui viewport inside its one window: `run` sets
  `Context::set_embed_viewports(true)` (egui's default, set again because the
  context comes from the program), so a viewport a program would show as a
  native window elsewhere appears as an egui window inside the main one.
- Text clipboard: Ctrl or Amiga + C, X, V go through clipboard.device unit 0
  (IFF FTXT, ISO 8859-1). `egui_aros::clipboard_text()` reads it directly.
- The busy pointer while egui's cursor is `CursorIcon::Wait`; `open_url` and
  egui's `OpenUrl` commands use the system's `OpenURL` command.
- With the `asl` feature: `egui_aros::asl::request` shows ASL file, multi-file,
  save and drawer requesters and returns absolute paths.

## Renderer

`Options::renderer` chooses at run time; the `glow` feature only decides whether
OpenGL is compiled in.

| `RendererRequest` | With `glow` | Without `glow` |
|---|---|---|
| `Cpu` (default) | CPU renderer; glutin, gl.library and Mesa are not touched | CPU renderer |
| `Auto` | Hardware OpenGL if a context can be made; CPU with the reason when there is none or it is a software rasterizer (softpipe, llvmpipe) | CPU, reason "built without OpenGL" |
| `Gpu` | OpenGL, or `run` returns `Error::Renderer(reason)` before any frame is drawn | `Error::Renderer` |

With `Gpu`, glutin may already have opened the window when the context fails; it
is closed again without a frame. `renderer_info()` returns what was chosen once
the window is open: the request, the renderer used (`RendererKind::Cpu` or
`OpenGl`), the reason for a fallback or failure, GL_VENDOR, GL_RENDERER,
GL_VERSION, the GLSL version, and `software`: `Some(true)` when GL_RENDERER
names a software rasterizer (softpipe, llvmpipe, swrast, "Software"),
`Some(false)` for a known hardware driver, `None` otherwise. The same is logged
as one line (`RendererInfo::summary`). `renderer_name()` gives a short text.

On AROS One 1.3 under QEMU the OpenGL renderer is Mesa softpipe, which draws on
the CPU and is slower than egui-aros's own CPU renderer; use it for function
checks, not for speed.

## Fonts

egui 0.37 builds its default fonts only with egui's `default_fonts` feature.
egui-aros does not enable it: enable it in the program's own `egui` dependency,
or install fonts with `Context::set_fonts`.

## What the program decides

- What happens when the close gadget is used: `App::close_requested` returns
  `false` to keep the window open (to ask about unsaved changes), and
  `App::quit_requested` or `ViewportCommand::Close` closes it later.
- Saving, and ending its worker threads and thread pools. AROS's pthread library
  waits for every thread when the process exits: end them before `main` returns.
  The library never calls `exit` and does not stop thread pools.

## Lifetimes

- `run` returns after the window has closed and the renderer's textures are
  freed. One `run` per process (winit allows one event loop per process).
- Repaint requests from other threads during the shutdown or after `run` has
  returned do nothing; the wake-up handle is disabled before the loop's signal
  is freed.
- `WindowHandle` (given to `App::started`) holds the window weakly: after the
  window has closed, its calls do nothing and `raw()` returns `None`.

## Measuring

`App::frame_done` receives a `FrameReport` for every frame shown: how long the
loop slept and why it woke, the time spent in the program's code, tessellation,
texture upload, rasterization and presentation, the input events since the
previous frame and their latency (from input.device's time stamp to the frame
being shown). `Options::measure_paint` adds the renderer's per-kind statistics.

## Known limitations

- Keymap text is read as ISO 8859-1. With a keymap that produces another 8-bit
  charset (the Polish `pc105_pl` produces ISO 8859-2), letters outside ASCII
  arrive as the wrong characters; text shown by the program is not affected.
- No pictures on the clipboard, no files dropped on the window, no pointer
  shapes other than the busy pointer, one window per program (other viewports
  are drawn inside it).
- No input method (winit's `reset_ime` does nothing on AROS) and no
  accessibility (AccessKit) support.
