# egui-aros

Experimental integration of [egui](https://github.com/emilk/egui) with AROS
for x86_64 ABIv11 (such as AROS One 1.3): an Intuition window, keyboard,
mouse and wheel input, repaint scheduling, the text clipboard and rendering
for egui programs, shared by several ports instead of being copied into each.

The first stage is a minimal eframe/glow program
([examples/glow-probe](examples/glow-probe)) that checks whether the standard
egui desktop stack (eframe, egui-winit, glutin, winit) runs on AROS on top of
the AROS crates that already exist upstream.

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
