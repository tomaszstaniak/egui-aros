# egui-aros

[egui](https://github.com/emilk/egui) integration for **AROS x86_64 ABIv11**. It provides an Intuition window, keyboard and mouse input, repaint scheduling, a text clipboard and a CPU renderer.

WordCraft, PhotoCraft, DeckCraft and GridCraft use this library. It is experimental; other CPUs and ABIs are not supported by these builds.

An optional `glow` feature provides OpenGL rendering through `gl.library`. The published Craft binaries use the CPU renderer.

## Using the library

- [API and integration guide](docs/using.md)
- [Building and tests](docs/development.md)
- [Source revisions and patches](docs/patching.md)

The library is in `crates/egui-aros`. The `examples/` directory contains CPU rendering, eframe/glow and lifecycle examples.

## Dependencies and credits

[upstreams.json](upstreams.json) pins `aros-gui-sys` from AROS, the winit, glutin and raw-window-handle changes from AROS contrib, and the original PhotoCraft Intuition shell from AROS ports. Changes to external code are stored in `patches/`.
