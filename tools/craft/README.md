# Craft port tools

One implementation of the workflow the Craft ports for AROS share: pinned upstream sources, a patch series, patched crates, builds for the AROS targets, release archives, and a trial of the series on a newer upstream commit. A port does not copy these files. It pins a commit of this repository and runs that snapshot.

| File | Purpose |
|---|---|
| `craft.py` | the commands: `config`, `fetch`, `deps`, `materialize`, `export`, `verify`, `crates`, `ctools`, `host`, `build`, `update-check`, `package` |
| `craftconfig.py` | settings of a port: environment, then its local file, then defaults; build targets |
| `update.py` | `update-check` |
| `package.py`, `icons.py` | `package binary` and `package source` |
| `bootstrap.py` | what a port keeps as its `scripts/craft.py` |
| `tests/` | `python3 -m unittest discover -s tools/craft/tests` |

## What a port keeps

- `scripts/craft.py`: a copy of `bootstrap.py`, unchanged.
- `scripts/port.json`: `id`, `name`, `env_prefix`, `packages` (cargo package to AROS program), `tools` (test tools).
- `scripts/tooling.json`: the pin.

```json
{
  "url": "https://github.com/tomaszstaniak/egui-aros.git",
  "commit": "<full 40-character commit>",
  "path": "tools/craft",
  "tree_sha256": "<content hash of tools/craft at that commit>",
  "local_key": "egui_aros"
}
```

`python3 scripts/craft.py COMMAND` prepares the pinned snapshot in the port's ignored `build/tooling/`, compares its content with `tree_sha256`, and runs the command for the port. A snapshot with other content is refused, also a cached one that was edited later. A branch name is not accepted as `commit`.

The snapshot comes from the first of these that holds the commit: the checkout named by `CRAFT_TOOLING_SOURCE`; the checkout that the local file names under `local_key`; the `url`. With a local checkout no network is used.

The tools take the port's repository from the bootstrap, never from their own location, so one snapshot directory can serve any port.

## Changing the pin

1. Commit the change here and run the tests.
2. In the port: `python3 scripts/craft.py --tooling-hash <commit>` prints the commit and its `tree_sha256`; put both in `scripts/tooling.json`.
3. Run `config` and `verify`, compare a build manifest with the previous one, commit.

Each port moves when it chooses. An older release of a port keeps building with the tools it pinned.

`CRAFT_TOOLING_DIR=/path/to/tools/craft` runs a directory as it is on disk, for work on the tools. Builds made that way record `"pinned": false`, and `package source` refuses them.

## update-check

```
python3 scripts/craft.py update-check --upstream-commit <full commit> --output report.json [--keep NEW_DIR]
```

Tries the port's series on that commit in a directory of its own and writes a report: for each patch `applies`, `applies-3way`, `upstream-equivalent` or `conflicts` (with the files); `Cargo.lock` packages added, removed and changed; changed manifests; changed Git revisions in `[patch.crates-io]`; patched crates whose version the candidate no longer locks. `--keep` leaves the trial checkout, with the applying patches as commits, for resolving the rest.

It reads the port and changes nothing in it: pins, patches, the work copy and `upstream/<id>` stay as they were, and the candidate commit is fetched into the trial only. No patch is dropped. `upstream-equivalent` means the patch reverses cleanly on the candidate, which is a reason to check the behaviour before retiring it. Moving the pins is a manual step after review.
