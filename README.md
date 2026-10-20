# egui-aros

Experimental integration of [egui](https://github.com/emilk/egui) with AROS
for x86_64 ABIv11 (such as AROS One 1.3): an Intuition window, keyboard,
mouse and wheel input, repaint scheduling, the text clipboard and rendering
for egui programs, shared by several ports instead of being copied into each.

The library (`crates/egui-aros`) runs an egui program in an Intuition window with a CPU
renderer; an OpenGL renderer through gl.library is optional (feature `glow`). The AROS ports of
WordCraft, PhotoCraft, DeckCraft and GridCraft use it, pinned by commit. The examples
(`examples/`) are the probes it was measured with: a CPU-rendered scene, the same scene through
eframe and glow, and a lifecycle test.

## Start here

- [Documentation](docs/README.md)
- [Development](docs/development.md): toolchain, SDK, build and tests.

## Sources

`upstreams.json` pins the external sources: `aros-gui-sys` from the AROS
repository, the AROS changes for winit, glutin and raw-window-handle from the
contrib repository, and the PhotoCraft AROS shell from the ports repository.
They are applied unchanged; our own changes to external code are versioned
patch series under `patches/`.

## Scope

x86_64 ABIv11 only. Other ABIs and CPUs are untested.
