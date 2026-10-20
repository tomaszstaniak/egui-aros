# Candidates for aros-development-team/ports

Patches against `graphics/photocraft/photocraft-aros` at ports `685982ed770f3c687d2b5d828a7069c11d450c6c` (`upstreams.json`, id `ports`), in `series` order. They are not applied in this repository's build: egui-aros carries its own copy of the renderer (`crates/egui-aros/src/raster.rs`), where the same changes are made and tested. These are the same changes transcribed onto the original file, for review and possible contribution.

| Patch | Status | Verification |
|---|---|---|
| 0001 top-left fill rule | candidate, not submitted | test included; run in the egui-aros copy (fails without, passes with); not yet compiled in the ports tree |
| 0002 constant-UV triangles sampled once | candidate, not submitted | measured in the WordCraft AROS port (patch 0010 there); not yet compiled in the ports tree |
