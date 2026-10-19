---
type: guide
updated: YYYY-MM-DD
---

# Reproducing and saving upstream changes

{{Document the real manifest, pinned sources, series order, prerequisites, bootstrap,
save, and isolated-reproduction commands. Mark tools not yet implemented explicitly.}}

Sources are configured in `upstreams.json`; changes and their rationale are kept in
`patches/`. An empty series represents the unchanged pin. Preserve local work and Git
history before recreating checkouts. Do not infer safety from a clean tree alone.

Published instructions and patch verification summaries must stand on their own.
Private run reports and screenshots belong in private `<workspace>/docs/` outside repository Git; do not link to them
as a prerequisite for reproducing this project from a public clone.
