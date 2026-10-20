# Preserved source changes

| Directory | Applies to | Applied by |
|---|---|---|
| `crates/` | crates.io crates and pinned upstream paths named in `crates/crates.json`; series entries `<id>:<path>` are applied unchanged from a pinned upstream (the contrib AROS diffs), other entries are ours | `scripts/ea.py crates`, `build`, `vendor` |
| `libc-aros/` | rust-aros `std-groundwork/libc-aros` at the commit in `scripts/eaconfig.py` | `scripts/ea.py build` |
| `aros-pthread/` | deadwood2/AROS `compiler/pthread` at the commit in `scripts/eaconfig.py` | `scripts/ea.py build` |
| `ports/` | aros-development-team/ports, `graphics/photocraft/photocraft-aros`: candidates for that repository, not applied in this build (see its README) | review only |

Every patch carries its problem, rationale, scope, upstream status, verification and removal condition in its header or commit message. `libc-aros/`, `aros-pthread/`, `crates/rayon-core-*` and `crates/parking_lot_core-*` are the PhotoCraft AROS port's patches, copied unchanged; their headers name that port. See [the patch workflow](../docs/patching.md).
