#!/usr/bin/env python3
"""Runs the Craft port tools that scripts/tooling.json pins, for this repository.

The tools (fetch, materialize, export, verify, crates, build, host, update-check; see
`craft.py --help`) are shared by the Craft ports and live in egui-aros under tools/craft. This
file is the same in every port (a copy of tools/craft/bootstrap.py): it prepares the exact pinned
snapshot in the ignored build/tooling/, checks its content against the recorded hash, and hands
it this repository and the command line. Nothing is taken from a branch head.

scripts/tooling.json:
  url          repository holding the tools
  commit       full 40-character commit
  path         directory of the tools in that repository
  tree_sha256  content hash of that directory at that commit (`--tooling-hash COMMIT` prints it)
  local_key    optional: a key of the local file (local.json, or the file <PREFIX>_LOCAL names)
               whose value is a local checkout of that repository

Where the snapshot comes from, first that has the commit: the checkout CRAFT_TOOLING_SOURCE
names; the checkout the local file names; the url. With a local checkout no network is used.
CRAFT_TOOLING_DIR runs a tools directory as it is on disk, for work on the tools themselves; builds
made that way record the tools as unpinned.
"""
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]


class BootstrapError(Exception):
    pass


def _git(*args, check=True):
    result = subprocess.run(["git", "-c", "gc.auto=0", "-c", "maintenance.auto=false", *map(str, args)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if check and result.returncode:
        raise BootstrapError(f"git {' '.join(map(str, args))}: {result.stderr.decode(errors='replace').strip()}")
    return result


def tree_hash(root):
    """Sorted relative name, NUL, mode bit for execute, content SHA-256 of every regular file
    (interpreter caches left out)."""
    root = Path(root)
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if p.is_file() and "__pycache__" not in p.relative_to(root).parts):
        executable = b"x" if os.access(path, os.X_OK) else b"-"
        digest.update(path.relative_to(root).as_posix().encode() + b"\0" + executable + hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def _has_commit(checkout, commit):
    return _git("-C", checkout, "cat-file", "-e", commit + "^{commit}", check=False).returncode == 0


def _extract(checkout, commit, path, dest):
    data = _git("-C", checkout, "archive", "--format=tar", f"{commit}:{path}").stdout
    dest.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        tar.extractall(dest, filter="data")


def tree_hash_of_commit(checkout, commit, path):
    with tempfile.TemporaryDirectory(prefix="tooling-hash-") as tmp:
        _extract(checkout, commit, path, Path(tmp) / "tree")
        return tree_hash(Path(tmp) / "tree")


def _local_checkout(spec):
    key = spec.get("local_key")
    if not key:
        return None
    try:
        prefix = json.loads((REPO / "scripts" / "port.json").read_text())["env_prefix"]
    except (OSError, ValueError, KeyError):
        return None
    local = Path(os.environ.get(prefix + "_LOCAL") or REPO / "local.json")
    try:
        value = json.loads(local.read_text()).get(key)
    except (OSError, ValueError):
        return None
    return (local.parent / value).resolve() if value else None


def _fetch(url, commit, into):
    """The commit from the url, in a repository of its own under `into`; None when it is not there."""
    _git("init", "--quiet", into)
    if _git("-C", into, "fetch", "--quiet", "--depth", "1", url, commit, check=False).returncode == 0 and _has_commit(into, commit):
        return into
    # A server that only hands out branch tips: take the branches and look for the commit.
    if _git("-C", into, "fetch", "--quiet", url, "+refs/heads/*:refs/remotes/source/*", check=False).returncode == 0 and _has_commit(into, commit):
        return into
    return None


def read_pin():
    path = REPO / "scripts" / "tooling.json"
    try:
        spec = json.loads(path.read_text())
    except (OSError, ValueError) as error:
        raise BootstrapError(f"{path}: {error}") from error
    for key in ("url", "commit", "path", "tree_sha256"):
        if not isinstance(spec.get(key), str) or not spec[key]:
            raise BootstrapError(f"{path}: `{key}` is missing")
    if not re.fullmatch(r"[0-9a-f]{40}", spec["commit"]):
        raise BootstrapError(f"{path}: `commit` must be a full 40-character commit, not a name that can move")
    if not re.fullmatch(r"[0-9a-f]{64}", spec["tree_sha256"]):
        raise BootstrapError(f"{path}: `tree_sha256` must be 64 hexadecimal characters")
    if spec["path"].startswith(("/", "-")) or ".." in Path(spec["path"]).parts:
        raise BootstrapError(f"{path}: `path` must stay inside the repository")
    return spec


def snapshot():
    """The directory of the pinned tools and the record of how they were obtained."""
    spec = read_pin()
    record = {"url": spec["url"], "commit": spec["commit"], "path": spec["path"], "tree_sha256": spec["tree_sha256"], "pinned": True}
    live = os.environ.get("CRAFT_TOOLING_DIR")
    if live:
        print(f"craft.py: tools from {live} as they are on disk (unpinned; not for a release)", file=sys.stderr)
        return Path(live).resolve(), dict(record, pinned=False, tree_sha256=tree_hash(live))
    cache_root = REPO / "build" / "tooling"
    cache = cache_root / f"{spec['commit'][:12]}-{spec['tree_sha256'][:12]}"
    if cache.exists():
        got = tree_hash(cache)
        if got != spec["tree_sha256"]:
            raise BootstrapError(f"{cache} has content {got}, scripts/tooling.json records tree_sha256 {spec['tree_sha256']}; "
                                 "it was changed after it was prepared. Remove that directory to prepare it again.")
        return cache, record
    cache_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="prepare-", dir=cache_root) as tmp:
        source = None
        named = os.environ.get("CRAFT_TOOLING_SOURCE")
        if named:
            if not _has_commit(named, spec["commit"]):
                raise BootstrapError(f"CRAFT_TOOLING_SOURCE={named} has no commit {spec['commit']}")
            source = named
        if source is None:
            local = _local_checkout(spec)
            if local and _has_commit(local, spec["commit"]):
                source = local
        if source is None:
            source = _fetch(spec["url"], spec["commit"], Path(tmp) / "fetch")
        if source is None:
            raise BootstrapError(f"commit {spec['commit']} of the tools is not available from {spec['url']}. Offline, set "
                                 "CRAFT_TOOLING_SOURCE to a local checkout of that repository"
                                 + (f", or `{spec['local_key']}` in the local file" if spec.get("local_key") else "") + ".")
        staged = Path(tmp) / "tree"
        _extract(source, spec["commit"], spec["path"], staged)
        got = tree_hash(staged)
        if got != spec["tree_sha256"]:
            raise BootstrapError(f"{spec['path']} at {spec['commit']} has content {got}, scripts/tooling.json records tree_sha256 "
                                 f"{spec['tree_sha256']}; nothing was prepared")
        try:
            staged.rename(cache)
        except OSError:
            # Another run prepared the same snapshot meanwhile; it is checked like any cached one.
            if not cache.exists() or tree_hash(cache) != spec["tree_sha256"]:
                raise
    return cache, record


def tools():
    """Prepares the pinned tools and makes their modules importable (`import craftconfig`, then
    `craftconfig.configure(REPO)`), for the port's other scripts. Returns (directory, record)."""
    sys.dont_write_bytecode = True
    path, record = snapshot()
    if str(path) not in sys.path:
        sys.path.insert(0, str(path))
    return path, record


def main(argv):
    try:
        if argv[:1] == ["--tooling-hash"] and len(argv) == 2:
            spec = read_pin()
            source = os.environ.get("CRAFT_TOOLING_SOURCE") or _local_checkout(spec)
            if not source:
                raise BootstrapError("set CRAFT_TOOLING_SOURCE to a local checkout of the tools' repository")
            commit = _git("-C", source, "rev-parse", "--verify", argv[1] + "^{commit}").stdout.decode().strip()
            print(json.dumps({"commit": commit, "tree_sha256": tree_hash_of_commit(source, commit, spec["path"])}, indent=2))
            return 0
        path, record = tools()
    except (BootstrapError, OSError) as error:
        print(f"craft.py: {error}", file=sys.stderr)
        return 1
    module = importlib.util.spec_from_file_location("craft_tools", path / "craft.py")
    craft = importlib.util.module_from_spec(module)
    sys.modules["craft_tools"] = craft
    module.loader.exec_module(craft)
    return craft.main(REPO, argv, tooling=record)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
