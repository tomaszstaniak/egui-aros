# egui-aros

[egui](https://github.com/emilk/egui) integration for **AROS x86_64 ABIv11**. It provides an Intuition window, keyboard and mouse input in the system character set, repaint scheduling, a clipboard for text and pictures, a CPU renderer with an optional OpenGL renderer chosen at run time, and program start-up helpers (`aros-startup`, also for command-line programs).

WordCraft, VectorCraft, PhotoCraft, DeckCraft and GridCraft use this library. It is experimental. The Craft ports build it for x86_64 ABIv11, x86_64 ABIv1 and the Raspberry Pi (aarch64 ABIv1).

An optional `glow` feature provides OpenGL rendering through `gl.library`. The published Craft binaries use the CPU renderer.

## Using the library

- [API and integration guide](docs/using.md)
- [Building and tests](docs/development.md)
- [Source revisions and patches](docs/patching.md)

The library is in `crates/egui-aros`, the start-up helpers in `crates/aros-startup`. The `examples/` directory contains CPU rendering, eframe/glow and lifecycle examples, and `clip-probe` and `stack-probe`, native checks of the clipboard and the minimum stack.

## Dependencies and credits

[upstreams.json](upstreams.json) pins `aros-gui-sys` from AROS, the winit, glutin and raw-window-handle changes from AROS contrib, and the original PhotoCraft Intuition shell from AROS ports. Changes to external code are stored in `patches/`.
