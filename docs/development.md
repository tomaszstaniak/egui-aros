# Development

## What you need

- An x86_64 AROS **ABIv11** cross toolchain (GCC 10.5 `x86_64-aros-gcc`, `-ar`)
  and the matching ABIv11 SDK. The library's C code and the final link must use
  the same SDK; it is passed to the compiler as `--sysroot`.
- A Rust toolchain that can build `std` for AROS, with
  [rust-aros](https://github.com/tomaszstaniak/rust-aros)'s target template and
  `libc-aros` (the build uses the commit named in `scripts/eaconfig.py`).
  `rustc --version` must match `rustc_version` there.

  **Current limitation:** that rust-aros commit and the matching std toolchain
  are not published yet, so this repository cannot yet be built from public
  sources alone.
- A deadwood2/AROS checkout at the commit in `scripts/eaconfig.py`: its
  `compiler/pthread` is rebuilt with the fix in `patches/aros-pthread`.
- Python 3.11+, Git, `patch`.

Copy `local.example.json` to `local.json` and fill in the paths (or set the
`EGUI_AROS_*` variables listed by `scripts/ea.py config`).

## Build

```sh
scripts/ea.py fetch                  # sparse clones of the pinned upstreams
scripts/ea.py build --online         # first build: downloads crates
scripts/ea.py build                  # later builds, offline
scripts/ea.py build -p egui-aros-cpu-probe
```

Each build writes `build/v11/out/<date>-<id>/` with the binaries and
`manifest.json` (sources, patches, toolchain, the exact cargo command and the
packages linked). A build is not a test: run the result on AROS.

`cargo` prints "patch ... was not used in the crate graph" for every patched
crate: that comes from building the standard library, which sees the same
`[patch]` table. The manifest's `linked_packages` shows which copies were used.

## Host tests

```sh
scripts/ea.py test
```

Runs the library's tests on the build machine (the renderer's coverage tests,
text conversions), with the patched crates in place.

## Examples

| Package | What it is |
|---|---|
| `egui-aros-cpu-probe` | a small scene (text, edit fields, scrolling, a translucent texture) on egui-aros; feature `glow` adds OpenGL, `gl-failure-test` adds a context failure on request |
| `egui-aros-glow-probe` | the same scene on eframe's glow renderer through glutin's GLA backend |
| `egui-aros-lifecycle-probe` | logs the task's signals around each winit/glutin step |

Both probes take `EGUI_PROBE_SIZE=<w>x<h>`, `EGUI_PROBE_FRAMES=<n>` (repaint
continuously and close after n frames) and `EGUI_PROBE_REPORT=<file>` (frame
times and the task's signals at start and end); the CPU probe also takes
`EGUI_PROBE_WAKER=1` (another thread requests repaints every millisecond, also
after the window has closed) and `EGUI_PROBE_RENDERER=cpu|auto|gpu` (the
`RendererRequest`; default `auto` when built with `glow`, else `cpu`). A CPU probe
built with `--features gl-failure-test` makes the OpenGL context fail when
`EGUI_AROS_TEST_GL_FAILURE` is set, to check the fallback and the error on a
system where OpenGL works. Give them `Stack 4000000` in the Shell.
`tests/guest/` has AmigaDOS scripts that run them.

## Using egui-aros from another repository

`scripts/ea.py vendor --dest DIR --cargo-home HOME --lock Cargo.lock` prepares
this tree's crate and the patched crates it needs under `DIR` and writes
`DIR/patches.json`; a program's build adds each path as a `[patch.crates-io]`
entry. Pin the egui-aros commit in the program's own records and run `vendor`
from that commit (with `EGUI_AROS_UPSTREAM_ROOT` naming checkouts made by
`ea.py fetch` when the commit is extracted elsewhere).

On this branch every egui crate comes from the pinned egui 0.37 workspace:
ecolor, emath, epaint, epaint_default_fonts, egui, egui-winit, egui_glow and
eframe always, and egui_extras, egui_system_font_provider, egui_inspection,
egui_demo_lib, egui-wgpu and egui_kittest when the program's lock uses them, so
that no egui crate is left on another source. winit is the pinned winit fork with
the AROS backend. A program whose `[patch.crates-io]` names these crates as Git
dependencies replaces those entries with the paths from `patches.json` (Cargo
also lets `--config patch.crates-io.<name>.path=...` take precedence).
