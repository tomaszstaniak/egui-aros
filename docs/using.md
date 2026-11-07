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
- Text in the system character set: the set Locale preferences name in
  `ENV:CHARSET` (codesets.library), else codesets.library's default, else
  ISO 8859-1. Keymap text, window titles and requester texts are converted with
  it; `egui_aros::system_charset()` tells which set is in use.
- Text clipboard: Ctrl or Amiga + C, X, V go through clipboard.device unit 0
  (IFF FTXT). Text the system set holds is written in it without a CSET chunk,
  so every AROS program reads it; other text is written as UTF-8 with CSET 106
  (as TextEditor.mcc and BetterString.mcc read it). `egui_aros::clipboard_text()`
  and `set_clipboard_text()` read and write it directly.
- Pictures on the clipboard: Ctrl or Amiga + V with a picture and no text sends
  `Event::PasteImage`. `egui_aros::clipboard_image()` reads the picture through
  datatypes (any format a picture datatype reads; `ClipboardImage::Pixels` as
  straight RGBA8, `Unreadable` with the reason, `None` for an empty or text
  clipboard; at most 16384 pixels a side and 256 MiB). `set_clipboard_image()`
  writes a 24-bit IFF ILBM (alpha is not written). Reading never changes the
  clipboard.
- The busy pointer while egui's cursor is `CursorIcon::Wait`; `open_url` and
  egui's `OpenUrl` commands use the system's `OpenURL` command.
- With the `asl` feature: `egui_aros::asl::request` shows ASL file, multi-file,
  save and drawer requesters and returns absolute paths.

## Start-up (`egui_aros::startup`, the `aros-startup` crate)

- `minimum_stack!(bytes)` declares the program's minimum stack (`__stack`); the
  start-up code then runs the program on a stack of that size when it is started
  from a Shell with a smaller one (40,960 bytes in an AROS One Shell).
  Returning from `main` and `exit()` both work.
- `startup(keys)` returns a Workbench start's documents (as -nix paths) and the
  requested ToolTypes; `program_dir()`, `absolute(name)`, `dos_to_nix`,
  `nix_to_dos` convert names; `quiet_dos()` turns "insert volume" requesters
  into errors; `show_error` shows a requester; `task_names()` lists the system's
  tasks (for finding threads that keep a program from ending: AROS waits for
  every thread at exit).
- The crate has no egui dependency, so command-line programs use it too.

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

`egui_aros::launch::choose` takes the choice from `--gui-renderer`, the
Workbench ToolType `GUI_RENDERER`, the `GUI_RENDERER` environment variable and
the program's default, in that order; `launch::effective` keeps AUTO on the CPU
on the Raspberry Pi, where OpenGL is not verified.

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

- A UTF-8 name returned by a requester is used as it is; a name in the system
  character set is shown right, but Rust's std opens its UTF-8 form, which is a
  different name on disk.
- No files dropped on the window, no pointer
  shapes other than the busy pointer, one window per program (other viewports
  are drawn inside it).
- No input method (winit's `reset_ime` does nothing on AROS) and no
  accessibility (AccessKit) support.
