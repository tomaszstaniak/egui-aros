# Patching

`upstreams.json` names each external source with its URL and full commit:

| Id | Used for |
|---|---|
| `aros` | `tools/crosstools/rust/crates/aros-gui-sys`: the C glue for Intuition windows, input, signals, clipboard, ASL and GLA |
| `contrib` | `development/rust/crates`: the AROS diffs for winit, glutin and raw-window-handle |
| `ports` | `graphics/photocraft/photocraft-aros`: the origin of the CPU renderer |

`scripts/ea.py fetch` makes clean, sparse checkouts in `upstream/<id>`; they are
never edited. Changes to external code are patch files in this repository:

- `patches/crates/<name>-<version>/series` lists, in order, the patches applied
  to a crate before it is built. An entry `contrib:<path>` is a file of the
  pinned contrib checkout, applied unchanged; other entries are our patches,
  stored beside the series. `patches/crates/crates.json` names the crates, the
  checksum of each crates.io archive, or the upstream path a crate comes from.
- `patches/libc-aros`, `patches/aros-pthread`: changes to rust-aros's libc and to
  AROS's pthread library, applied by the build.
- `patches/ports`: candidates for the ports repository, kept for review; the
  build does not use them.

Every patch says what problem it solves, why, its scope, its upstream status,
how it was verified and when it can be removed.
