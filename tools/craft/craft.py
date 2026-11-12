#!/usr/bin/env python3
"""A Craft AROS port: fetch pinned sources, rebuild the work copy from the patch series, prepare
patched crates, and build for the AROS targets in craftconfig.py. Shared by the Craft ports: a
port's scripts/craft.py runs the snapshot of these tools that its scripts/tooling.json pins and
passes its own repository; the app is named in the port's scripts/port.json.

Commands (run `craft.py COMMAND --help`):
  config       show where each setting comes from
  fetch        clone/verify the pinned upstream checkouts (upstream/<id>)
  materialize  create work/<app> from the pin and patches/<app>/series
  export       write the work copy's commits back to patches/<app>
  verify       rebuild the series in a new temporary directory, compare trees
  crates       prepare the patched crates.io crates (checksummed, patched)
  build        build the AROS binaries from the work copy into a fresh output
  update-check try the series on another upstream commit, apart from the port; report only
  package      make the release archives from build outputs (`craft.py package --help`)

Nothing here edits upstream/<id>, a shared toolchain, an SDK or the Cargo
registry. Existing work copies are never reset or recreated.
"""
import argparse
import datetime
import hashlib
import io
import json
import os
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import craftconfig  # noqa: E402

# The SDK's libpthread.a objects are reproduced exactly by these flags (checked below).
PTHREAD_CFLAGS = ["-O2", "-mcmodel=large", "-fno-omit-frame-pointer"]
# The members patches/aros-pthread changes; only these are linked from the private build.
PTHREAD_PATCHED = ("pthread", "pthread_create")
# aarch64: the SDK's ThreadInfo layout, read from its libpthread.a disassembly (stride 0x390, task
# 0x28, detached 0x384) in rust-aros ports/runtime/build-arm-detach.py; the source must agree.
PTHREAD_LAYOUT_RASPI = {"sizeof(ThreadInfo)": 912, "offsetof(ThreadInfo, finished)": 32,
                        "offsetof(ThreadInfo, task)": 40, "offsetof(ThreadInfo, detached)": 900}
# The C bridge the aarch64 std PAL (Jonx's, in the common sysroot) calls; rust-aros
# tools/build-acceptance.py builds the same seven files.
GLUE_SOURCES = ("aros_fs_glue", "aros_thread_glue", "aros_sync_glue", "aros_env_glue",
                "aros_process_glue", "aros_proc_glue", "aros_net_glue")

# crates.io crates patched the same way for every Craft port (see crate_specs).
SHARED_CRATES_DIR = Path(__file__).resolve().parent / "crates"
# Patches to platform sources built for every port (rust-aros libc-aros, the AArch64 C bridge).
SHARED_PLATFORM_DIR = Path(__file__).resolve().parent / "platform"
# Set by configure() for the port being worked on; never derived from this file's location.
REPO = UPSTREAMS = PORT = APP = SERIES_DIR = CRATES_DIR = LIBC_DIR = PTHREAD_DIR = GLUE_DIR = WORK = None
# Test tools built with the same profile: name -> (manifest directory, binary).
TOOLS = {}
# Package -> the AROS binary it produces.
PACKAGES = {}
COMMITTER = []
# How these tools were obtained (the port's bootstrap passes it): recorded with every build.
TOOLING = None


def platform_patches(name):
    """The directory of a platform patch set (libc-aros, aros-glue): the shared one beside this
    file, the same for every port. A port that needs its own names the set under
    `own_platform_patches` in port.json; a copy in the port that is not named there is an error,
    so that neither copy is used without anyone having chosen it."""
    own = REPO / "patches" / name
    if name in PORT.get("own_platform_patches", []):
        if not (own / "series").is_file():
            raise craftconfig.ConfigError(f"port.json names its own {name} patches, but {own}/series is missing")
        return own
    if own.exists():
        raise craftconfig.ConfigError(f"{own} exists and the shared tools have the {name} patches too: remove the port's copy, "
                                      f"or name `{name}` under own_platform_patches in scripts/port.json")
    return SHARED_PLATFORM_DIR / name


def configure(repo, tooling=None):
    """Names the port (see craftconfig.configure) and derives its directories."""
    global REPO, UPSTREAMS, PORT, APP, SERIES_DIR, CRATES_DIR, LIBC_DIR, PTHREAD_DIR, GLUE_DIR, WORK, TOOLS, PACKAGES, COMMITTER, TOOLING
    craftconfig.configure(repo)
    REPO = craftconfig.REPO
    UPSTREAMS = REPO / "upstreams.json"
    PORT = craftconfig.PORT
    APP = PORT["id"]
    SERIES_DIR = REPO / "patches" / APP
    CRATES_DIR = REPO / "patches" / "crates"
    PTHREAD_DIR = REPO / "patches" / "aros-pthread"
    LIBC_DIR = platform_patches("libc-aros")
    GLUE_DIR = platform_patches("aros-glue")
    WORK = REPO / "work" / APP
    TOOLS = {k: tuple(v) for k, v in PORT.get("tools", {}).items()}
    PACKAGES = PORT["packages"]
    COMMITTER = ["-c", f"user.name={PORT['name']} AROS port", "-c", f"user.email={APP}-aros@invalid"]
    TOOLING = tooling


class Failure(Exception):
    pass


def run(cmd, cwd=None, env=None, capture=True, check=True, input=None):
    if str(cmd[0]) == "git":
        cmd = ["git", "-c", "gc.auto=0", "-c", "maintenance.auto=false", *cmd[1:]]
    result = subprocess.run([str(c) for c in cmd], cwd=cwd, env=env, input=input,
                            stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE if capture else None)
    if check and result.returncode != 0:
        err = (result.stderr or b"").decode(errors="replace").strip()
        raise Failure(f"command failed ({result.returncode}): {' '.join(map(str, cmd))}\n{err}")
    return result


def git(repo, *args, **kw):
    return run(["git", "-C", repo, *args], **kw).stdout.decode().strip()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def tree_hash(root):
    """Sorted relative name, NUL, mode bit for execute, content SHA-256 of every regular file."""
    root = Path(root)
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if p.is_file() and ".git" not in p.relative_to(root).parts):
        rel = path.relative_to(root).as_posix().encode()
        executable = b"x" if os.access(path, os.X_OK) else b"-"
        digest.update(rel + b"\0" + executable + bytes.fromhex(sha256_file(path)))
    return digest.hexdigest()


def upstreams():
    return json.loads(UPSTREAMS.read_text())["repositories"]


def series(series_dir=None):
    series_dir = Path(series_dir or SERIES_DIR)
    names = [line.strip() for line in (series_dir / "series").read_text().splitlines()
             if line.strip() and not line.startswith("#")]
    for name in names:
        if not (series_dir / name).is_file():
            raise Failure(f"series names a missing patch: {name}")
    return names


def check_upstream(rid, spec):
    path = REPO / "upstream" / rid
    head = git(path, "rev-parse", "HEAD")
    if head != spec["commit"]:
        raise Failure(f"upstream/{rid} is at {head}, the pin is {spec['commit']}")
    if git(path, "status", "--porcelain", "--untracked-files=all"):
        raise Failure(f"upstream/{rid} has local changes; it must stay a clean checkout")
    return path


# ---------------------------------------------------------------- commands

def cmd_deps(args):
    cfg = craftconfig.load(["cargo_home", "toolchain"])
    source = check_upstream(APP, upstreams()[APP])
    run(["cargo", "+" + cfg["toolchain"], "fetch", "--locked", "--manifest-path", source / "Cargo.toml"],
        env=dict(os.environ, CARGO_HOME=cfg["cargo_home"]), capture=False)


def cmd_config(args):
    print(craftconfig.describe())


def cmd_fetch(args):
    for rid, spec in upstreams().items():
        if rid == "egui-aros" or spec.get("prepared_by") == "egui-aros":
            # egui-aros is built from the configured checkout at the pinned commit (see
            # egui_aros_vendor); the egui and winit forks are prepared by its own fetch.
            continue
        path = REPO / "upstream" / rid
        if not path.exists():
            path.parent.mkdir(exist_ok=True)
            run(["git", "clone", "--quiet", spec["url"], path], capture=False)
            run(["git", "-C", path, "-c", "advice.detachedHead=false", "checkout", "--quiet", "--detach", spec["commit"]])
        url = git(path, "remote", "get-url", "origin")
        if url != spec["url"]:
            raise Failure(f"upstream/{rid} comes from {url}, expected {spec['url']}")
        check_upstream(rid, spec)
        print(f"upstream/{rid}: {spec['commit']} clean")


def apply_series(dest, base_repo, commit, series_dir=None):
    series_dir = Path(series_dir or SERIES_DIR)
    run(["git", "clone", "--quiet", "--no-checkout", base_repo, dest])
    run(["git", "-C", dest, "-c", "advice.detachedHead=false", "checkout", "--quiet", "-b", "aros", commit])
    for name in series(series_dir):
        result = run(["git", *COMMITTER, "-C", dest, "am", "--quiet", "--keep-cr", "--whitespace=nowarn",
                      "--committer-date-is-author-date", series_dir / name], check=False)
        if result.returncode != 0:
            run(["git", "-C", dest, "am", "--abort"], check=False)
            raise Failure(f"{name} does not apply:\n{result.stderr.decode(errors='replace')}")


def cmd_materialize(args):
    spec = upstreams()[APP]
    base = check_upstream(APP, spec)
    if WORK.exists() or WORK.is_symlink():
        raise Failure(f"{WORK} exists; it is never recreated. Use `verify` to compare it with the series, "
                      "or move it aside yourself after preserving its branches, stashes and untracked files.")
    WORK.parent.mkdir(exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix="materialize-", dir=WORK.parent))
    try:
        apply_series(staging / APP, base, spec["commit"])
        (staging / APP).rename(WORK)
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    print(f"{WORK}: {len(series())} patches applied on {spec['commit']}")


def work_state(work):
    status = git(work, "status", "--porcelain", "--untracked-files=all")
    return git(work, "rev-parse", "HEAD"), status


def export_series(work, base_repo, base_commit, series_dir, owner_repo):
    """Replaces `series_dir` with the work copy's commits since `base_commit`.

    Refuses when the work copy or `series_dir` (in the owning repository `owner_repo`) has
    uncommitted, staged or untracked changes: hand-edited headers or candidate patches would
    otherwise be lost. The new series is written beside the old one and must rebuild the work
    copy's tree from `base_repo` before it replaces anything; the swap is two renames, and the
    old directory is put back if the second fails."""
    work, series_dir, owner_repo = Path(work), Path(series_dir), Path(owner_repo)
    head, status = work_state(work)
    if status:
        raise Failure("the work copy has uncommitted or untracked changes; commit them first:\n" + status)
    if run(["git", "-C", work, "merge-base", "--is-ancestor", base_commit, "HEAD"], check=False).returncode:
        raise Failure(f"the work copy's HEAD does not descend from the pin {base_commit}")
    rel = series_dir.resolve().relative_to(owner_repo.resolve())
    dirty = git(owner_repo, "status", "--porcelain", "--untracked-files=all", "--", str(rel))
    if dirty:
        raise Failure(f"{rel} has uncommitted, staged or untracked changes; commit or move them first:\n{dirty}")
    count = int(git(work, "rev-list", "--count", f"{base_commit}..HEAD"))
    work_tree = git(work, "rev-parse", "HEAD^{tree}")
    tag = f"{os.getpid()}-{uuid.uuid4().hex[:8]}"
    staged = series_dir.parent / f".{series_dir.name}.new-{tag}"
    previous = series_dir.parent / f".{series_dir.name}.old-{tag}"
    staged.mkdir()
    try:
        run(["git", "-C", work, "format-patch", "--quiet", "--no-numbered", "--no-signature", "--full-index", "--binary",
             "--zero-commit", "-o", staged, f"{base_commit}..HEAD"])
        produced = sorted(staged.glob("*.patch"))
        if len(produced) != count:
            raise Failure(f"format-patch wrote {len(produced)} patches for {count} commits")
        (staged / "series").write_text("".join(p.name + "\n" for p in produced))
        for extra in series_dir.iterdir():
            if extra.name != "series" and extra.suffix != ".patch":
                shutil.copy2(extra, staged / extra.name)
        with tempfile.TemporaryDirectory(prefix="export check ") as tmp:
            apply_series(Path(tmp) / "rebuilt", base_repo, base_commit, staged)
            rebuilt = git(Path(tmp) / "rebuilt", "rev-parse", "HEAD^{tree}")
        if rebuilt != work_tree:
            raise Failure(f"the new series rebuilds tree {rebuilt}, not the work copy's {work_tree}")
        os.replace(series_dir, previous)
        try:
            os.replace(staged, series_dir)
        except OSError:
            os.replace(previous, series_dir)
            raise
    finally:
        if staged.exists():
            shutil.rmtree(staged)
    shutil.rmtree(previous)
    return head, count


def cmd_export(args):
    spec = upstreams()[APP]
    base = check_upstream(APP, spec)
    head, count = export_series(WORK, base, spec["commit"], SERIES_DIR, REPO)
    print(f"exported {count} patches from {head}; they rebuild its tree; review `git diff` and commit them")


def cmd_verify(args):
    spec = upstreams()[APP]
    base = check_upstream(APP, spec)
    with tempfile.TemporaryDirectory(prefix=f"{APP} verify ") as tmp:
        dest = Path(tmp) / "word craft"
        apply_series(dest, base, spec["commit"])
        rebuilt = git(dest, "rev-parse", "HEAD^{tree}")
    print(f"series rebuilt tree {rebuilt}")
    if not WORK.exists():
        return
    head, status = work_state(WORK)
    work_tree = git(WORK, "rev-parse", "HEAD^{tree}")
    print(f"work HEAD {head} tree {work_tree}{' (uncommitted changes present)' if status else ''}")
    if work_tree != rebuilt:
        raise Failure("the series does not reproduce the work copy's committed tree; run `export` and commit")
    print("series reproduces the work copy's committed tree (contents and modes)")


def crate_specs():
    """The patched crates of the port: [(spec, directory of its patches)].

    patches/crates/crates.json names crates of the shared set (crates/ beside this file) under
    `shared`, as `<name>-<version>`, and the port's own under `crates`. Nothing is chosen
    silently: a shared name that does not exist is an error, and so is a crate of the port's own
    that the shared set also has, unless its entry says `"replaces_shared": true`."""
    path = CRATES_DIR / "crates.json"
    if not path.is_file():
        return []
    data = json.loads(path.read_text())
    shared = {f"{s['name']}-{s['version']}": s for s in json.loads((SHARED_CRATES_DIR / "crates.json").read_text())["crates"]}
    out, seen = [], set()
    for key in data.get("shared", []):
        if key not in shared:
            raise Failure(f"{path}: `{key}` is not in the shared crate set ({', '.join(sorted(shared))})")
        if key in seen:
            raise Failure(f"{path}: `{key}` is named twice")
        seen.add(key)
        out.append((shared[key], SHARED_CRATES_DIR / key))
    for spec in data.get("crates", []):
        key = f"{spec['name']}-{spec['version']}"
        if key in seen and not spec.get("replaces_shared"):
            raise Failure(f"{path}: `{key}` is both under `shared` and the port's own")
        if key in shared and key not in seen and not spec.get("replaces_shared"):
            raise Failure(f"{path}: `{key}` is in the shared crate set; name it under `shared`, or mark the port's own "
                          "entry `\"replaces_shared\": true`")
        out = [(s, d) for s, d in out if f"{s['name']}-{s['version']}" != key]
        seen.add(key)
        out.append((spec, CRATES_DIR / key))
    return out


def cmd_crates(args, quiet=False):
    cfg = craftconfig.load(["cargo_home", "build_root"])
    out = {}
    for spec, patches in crate_specs():
        if spec.get("profiles") and cfg["profile"] not in spec["profiles"]:
            continue
        name = f"{spec['name']}-{spec['version']}"
        dest = Path(cfg["build_root"]) / "crates" / name
        if not dest.exists():
            archives = list(Path(cfg["cargo_home"]).glob(f"registry/cache/*/{name}.crate"))
            if len(archives) != 1:
                raise Failure(f"{name}.crate is not in {cfg['cargo_home']}; run `python3 scripts/craft.py deps` first")
            if sha256_file(archives[0]) != spec["checksum"]:
                raise Failure(f"{archives[0]} does not match the Cargo.lock checksum")
            dest.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix="crate-", dir=dest.parent) as tmp:
                with tarfile.open(archives[0]) as tar:
                    tar.extractall(tmp, filter="data")
                staged = Path(tmp) / name
                for patch in (patches / "series").read_text().split():
                    run(["patch", "--quiet", "-p1", "-i", patches / patch], cwd=staged)
                got = tree_hash(staged)
                if spec.get("tree_sha256") and got != spec["tree_sha256"]:
                    raise Failure(f"patched {name} tree {got} differs from the recorded {spec['tree_sha256']}")
                staged.rename(dest)
        got = tree_hash(dest)
        if spec.get("tree_sha256") and got != spec["tree_sha256"]:
            raise Failure(f"{dest} differs from the reviewed patch result; remove it to rebuild from the archive")
        out[spec.get("patch_key", spec["name"])] = {"package": spec["name"], "path": str(dest), "version": spec["version"], "tree_sha256": got}
        if not quiet:
            print(f"{name}: {dest} tree {got}{'' if spec.get('tree_sha256') else ' (not yet recorded)'}")
    return out


def sync_tree(files, dest):
    """Makes `dest` hold exactly `files` ({relative path: (bytes, executable)}), leaving
    unchanged files untouched so that cargo's fingerprints stay valid."""
    dest.mkdir(parents=True, exist_ok=True)
    for rel, (data, executable) in files.items():
        path = dest / rel
        if path.is_file() and path.read_bytes() == data and os.access(path, os.X_OK) == executable:
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_name(path.name + ".gc-tmp")
        tmp.write_bytes(data)
        tmp.chmod(0o755 if executable else 0o644)
        tmp.replace(path)
    wanted = set(files)
    for path in sorted(dest.rglob("*"), reverse=True):
        rel = path.relative_to(dest).as_posix()
        if rel == "Cargo.lock.build" or rel.startswith("target/"):
            continue
        if path.is_file() and rel not in wanted:
            path.unlink()
        elif path.is_dir() and not any(path.iterdir()):
            path.rmdir()


def snapshot(worktree):
    """The work copy's files: committed HEAD, or (worktree) the files on disk Git would track."""
    files = {}
    if worktree:
        names = run(["git", "-C", WORK, "ls-files", "-z", "--cached", "--others", "--exclude-standard"]).stdout.split(b"\0")
        for name in filter(None, names):
            path = WORK / name.decode()
            if path.is_file():
                files[name.decode()] = (path.read_bytes(), os.access(path, os.X_OK))
        return files
    data = run(["git", "-C", WORK, "archive", "--format=tar", "HEAD"]).stdout
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        for member in tar.getmembers():
            if member.isfile():
                files[member.name] = (tar.extractfile(member).read(), bool(member.mode & 0o111))
    return files


def egui_aros_vendor(cfg, root, src):
    """egui-aros at the commit pinned in upstreams.json, with its patched crates, prepared by its
    own `ea.py vendor` from that commit: {crate name: path} for the [patch] table, and the record.
    The checkout's upstream/ clones supply the sources its crate patches apply to."""
    spec = upstreams().get("egui-aros")
    if not spec:
        return {}, None
    checkout = Path(cfg["egui_aros"] or "")
    if not cfg["egui_aros"] or not (checkout / ".git").exists():
        raise Failure("upstreams.json pins egui-aros: set egui_aros in local.json to its checkout")
    commit = spec["commit"]
    if git(checkout, "cat-file", "-t", commit) != "commit":
        raise Failure(f"{checkout} has no commit {commit}")
    base = root / f"egui-aros-{commit[:12]}"
    tree = base / "src"
    if not tree.exists():
        data = run(["git", "-C", checkout, "archive", "--format=tar", commit]).stdout
        with tempfile.TemporaryDirectory(prefix="egui-aros-", dir=root) as tmp:
            with tarfile.open(fileobj=io.BytesIO(data)) as tar:
                tar.extractall(tmp, filter="data")
            shutil.copytree(tmp, tree)
    env = dict(os.environ, EGUI_AROS_UPSTREAM_ROOT=str(cfg["egui_upstream_root"] or checkout / "upstream"), EGUI_AROS_BUILD_ROOT=str(base / "build"),
               EGUI_AROS_LOCAL=str(base / "no-local.json"))
    out = run([sys.executable, tree / "scripts" / "ea.py", "vendor", "--dest", base / "vendor", "--cargo-home", cfg["cargo_home"],
               "--lock", src / "Cargo.lock"], env=env).stdout.decode()
    record = json.loads(out)
    record["commit"] = commit
    paths = {name: info["path"] for name, info in record["crates"].items()}
    paths["egui-aros"] = record["egui_aros"]
    return paths, record


def tool(cfg, name):
    """A binutils-style tool beside the compiler driver (x86_64-aros-gcc, aarch64-aros-clang)."""
    driver = cfg["aros_gcc"]
    for suffix in ("-gcc", "-clang"):
        if driver.endswith(suffix):
            return driver[: -len(suffix)] + "-" + name
    raise Failure(f"cannot derive {name} from the compiler driver {driver}")


def adjust_build_copy(cfg, src, libc, egui_aros):
    """Two changes to the build copy (never the work copy), recorded in the manifest:
    The app's own [patch.crates-io] takes egui and winit from storytold's Git forks, and a
    patch in Cargo.toml wins over a `--config` patch of the same crate, so those entries are
    removed for the crates egui-aros supplies; and the lock is moved to libc-aros's libc version,
    or Cargo leaves the libc patch unused and builds crates.io libc, which has no AROS module."""
    import re
    manifest = src / "Cargo.toml"
    text = manifest.read_text()
    removed = []
    for name in egui_aros:
        pattern = re.compile(rf'^{re.escape(name)} = \{{ git = "https://github.com/storytold/[^"]+", rev = "[0-9a-f]+" \}}\n', re.M)
        if pattern.search(text):
            text = pattern.sub("", text)
            removed.append(name)
    manifest.write_text(text)
    return {"removed_git_patches": removed}


def pin_libc(cfg, src, libc, config_args, record):
    """Move the build copy's lock to libc-aros's version (see adjust_build_copy). `config_args`
    are the build's `--config` patch entries: the lock cannot be resolved without them."""
    import re
    version = re.search(r'^version = "([^"]+)"', (libc / "Cargo.toml").read_text(), re.M).group(1)
    locked = sorted(set(re.findall(r'name = "libc"\nversion = "([^"]+)"', (src / "Cargo.lock").read_text())))
    for old in locked:
        if old != version:
            out = run(["cargo", "+" + cfg["toolchain"], "update", "--manifest-path", src / "Cargo.toml", *config_args,
                       "-p", f"libc@{old}", "--precise", version], env=dict(os.environ, CARGO_HOME=cfg["cargo_home"]))
            record.setdefault("libc_update_log", []).append(out.stderr.decode(errors="replace")[-2000:])
    record.update(libc_locked=locked, libc_used=version)


def objdump_key(cfg, obj):
    text = run([tool(cfg, "objdump"), "-d", "-r", "--no-show-raw-insn", obj]).stdout.decode().split("\n", 4)[-1]
    return hashlib.sha256(text.encode()).hexdigest()


def build_pthread(cfg, root):
    """libpthread.a from deadwood2/AROS compiler/pthread with patches/aros-pthread applied,
    in a private directory searched before the SDK. The unpatched tree is first checked to
    rebuild the SDK's archive object for object, so the patch is all that differs."""
    patches = [PTHREAD_DIR / n for n in (PTHREAD_DIR / "series").read_text().split()]
    commit = cfg["aros_src_commit"]
    key = hashlib.sha256(b"".join(bytes.fromhex(sha256_file(p)) for p in patches)).hexdigest()[:8]
    out = root / f"aros-pthread-{commit[:12]}-{key}"
    if (out / "pthread.json").exists():
        return out, json.loads((out / "pthread.json").read_text())
    archive = run(["git", "-C", cfg["aros_src"], "archive", "--format=tar", commit, "compiler/pthread"]).stdout
    gcc, ar = cfg["aros_gcc"], cfg["aros_ar"]
    with tempfile.TemporaryDirectory(prefix="pthread-", dir=root) as tmp:
        tmp = Path(tmp)
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(tmp, filter="data")
        src = tmp / "compiler/pthread"
        # The member list comes from the recipe, not a copy of it.
        recipe, names, collecting = (src / "mmakefile.src").read_text().splitlines(), [], None
        for line in recipe:
            if line.startswith(("LINKLIBFILES", "PTHREADFUNCS")):
                collecting = True
                line = line.split(":=", 1)[1]
            if collecting:
                names += [w for w in line.replace("\\", " ").split() if w]
                collecting = line.rstrip().endswith("\\")
        sdk_objs = tmp / "sdk"
        sdk_objs.mkdir()
        run([ar, "x", Path(cfg["aros_sdk"]) / "lib" / "libpthread.a"], cwd=sdk_objs)
        if sorted(p.stem for p in sdk_objs.glob("*.o")) != sorted(names):
            raise Failure(f"SDK libpthread.a members differ from the recipe's list {names}")
        for stage in ("plain", "patched"):
            if stage == "patched":
                for patch in patches:
                    run(["patch", "--quiet", "-p3", "-i", patch], cwd=src)
            objs = tmp / stage
            objs.mkdir()
            for name in names:
                run([gcc, f"--sysroot={cfg['aros_sdk']}", *PTHREAD_CFLAGS, "-I", src, "-c", src / f"{name}.c", "-o", objs / f"{name}.o"])
            if stage == "plain":
                mismatched = [n for n in names if objdump_key(cfg, objs / f"{n}.o") != objdump_key(cfg, sdk_objs / f"{n}.o")]
                if mismatched:
                    raise Failure(f"{commit} compiler/pthread does not rebuild the SDK's objects: {mismatched}")
        staged = tmp / "out"
        staged.mkdir()
        changed = [n for n in names if objdump_key(cfg, tmp / "patched" / f"{n}.o") != objdump_key(cfg, tmp / "plain" / f"{n}.o")]
        if sorted(changed) != sorted(PTHREAD_PATCHED):
            raise Failure(f"the pthread patches change {changed}, expected {list(PTHREAD_PATCHED)}")
        for n in PTHREAD_PATCHED:
            shutil.copyfile(tmp / "patched" / f"{n}.o", staged / f"{n}.o")
        record = {"aros_src_commit": commit, "patches": {p.name: sha256_file(p) for p in patches},
                  "cflags": PTHREAD_CFLAGS, "members": names, "unpatched_matches_sdk": True,
                  "sdk_archive_sha256": sha256_file(Path(cfg["aros_sdk"]) / "lib" / "libpthread.a"),
                  "objects": {f"{n}.o": sha256_file(staged / f"{n}.o") for n in PTHREAD_PATCHED}}
        (staged / "pthread.json").write_text(json.dumps(record, indent=2) + "\n")
        staged.rename(out)
    return out, record


def defined_symbols(cfg, obj):
    lines = run([tool(cfg, "nm"), "--defined-only", "--extern-only", obj]).stdout.decode().splitlines()
    return sorted(line.split()[-1] for line in lines if line.strip())


def build_pthread_raspi(cfg, root):
    """The two pthread members patches/aros-pthread changes, rebuilt for the native Pi SDK from
    mainline compiler/pthread at the target's commit, as rust-aros's ARM runtime work did
    (ports/runtime/build-arm-detach.py --fixed, same patch). That work showed the bug in the SDK's
    library on rpi-1 and the corrected objects passing. Clang does not rebuild the SDK's objects
    instruction for instruction (not claimed); instead the source's ThreadInfo layout is asserted
    against the SDK's, and the objects must define every symbol of the SDK members they replace."""
    # rebuilt: the members as the source has them (its pthread already has the fix), compiled here.
    patches = [] if cfg["pthread"] == "rebuilt" else [PTHREAD_DIR / n for n in (PTHREAD_DIR / "series").read_text().split()]
    commit = cfg["aros_src_commit"]
    cc, sdk_lib = cfg["aros_gcc"], Path(cfg["aros_sdk"]) / "lib" / "libpthread.a"
    # The SDK's library is part of the key: objects checked against one SDK say nothing about another.
    key = hashlib.sha256(b"".join(bytes.fromhex(sha256_file(p)) for p in patches + [sdk_lib])).hexdigest()[:8]
    out = root / f"aros-pthread-{commit[:12]}-{key}"
    if (out / "pthread.json").exists():
        return out, json.loads((out / "pthread.json").read_text())
    archive = run(["git", "-C", cfg["aros_src"], "archive", "--format=tar", commit, "compiler/pthread"]).stdout
    with tempfile.TemporaryDirectory(prefix="pthread-", dir=root) as tmp:
        tmp = Path(tmp)
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(tmp, filter="data")
        src = tmp / "compiler/pthread"
        layout = "#include \"pthread_intern.h\"\n#include <stddef.h>\n" + "".join(
            f'_Static_assert({expr} == {value}, "Pi SDK layout: {expr}");\n' for expr, value in PTHREAD_LAYOUT_RASPI.items())
        (tmp / "layout.c").write_text(layout)
        if patches:  # the recorded layout is the older Pi SDK's; `rebuilt` takes the SDK's own source revision
            run([cc, f"--sysroot={cfg['aros_sdk']}", "-O2", "-I", src, "-c", tmp / "layout.c", "-o", tmp / "layout.o"])
        sdk_objs = tmp / "sdk"
        sdk_objs.mkdir()
        run([cfg["aros_ar"], "x", sdk_lib, *[f"{n}.o" for n in PTHREAD_PATCHED]], cwd=sdk_objs)
        for patch in patches:
            run(["patch", "--quiet", "-p3", "-i", patch], cwd=src)
        staged = tmp / "out"
        staged.mkdir()
        for name in PTHREAD_PATCHED:
            run([cc, f"--sysroot={cfg['aros_sdk']}", "-O2", "-c", src / f"{name}.c", "-o", staged / f"{name}.o"])
            missing = sorted(set(defined_symbols(cfg, sdk_objs / f"{name}.o")) - set(defined_symbols(cfg, staged / f"{name}.o")))
            if missing:
                raise Failure(f"rebuilt {name}.o lacks symbols of the SDK's member, so -lpthread would still pull it: {missing}")
        record = {"aros_src_commit": commit, "patches": {p.name: sha256_file(p) for p in patches},
                  "cflags": ["-O2"], "layout_asserted": PTHREAD_LAYOUT_RASPI if patches else "not asserted (source revision of the SDK)", "unpatched_matches_sdk": "not claimed (Clang)",
                  "sdk_archive_sha256": sha256_file(sdk_lib),
                  "objects": {f"{n}.o": sha256_file(staged / f"{n}.o") for n in PTHREAD_PATCHED}}
        (staged / "pthread.json").write_text(json.dumps(record, indent=2) + "\n")
        staged.rename(out)
    return out, record


def sdk_pthread_record(cfg):
    """The SDK's own libpthread.a, linked as is (setting pthread = sdk)."""
    lib = Path(cfg["aros_sdk"]) / "lib" / "libpthread.a"
    return {"source": "SDK libpthread.a, unpatched (setting pthread = sdk: the SDK has mainline c31d3294)",
            "sdk_archive_sha256": sha256_file(lib)}


def build_glue(cfg, root):
    """Jonx's C bridge for the aarch64 std PAL, from a clean pomaros-hosted checkout at the pinned
    commit with patches/aros-glue applied, compiled with this build's compiler and SDK."""
    checkout, commit = Path(cfg["pomaros_hosted"]), cfg["pomaros_hosted_commit"]
    if git(checkout, "rev-parse", "HEAD") != commit or git(checkout, "status", "--porcelain"):
        raise Failure(f"{checkout} must be a clean checkout of {commit}")
    series = GLUE_DIR / "series"
    patches = [GLUE_DIR / n for n in series.read_text().split()] if series.exists() else []
    # Compiled against the SDK's headers: a different SDK is a different build; so are other patches.
    sdk_include = tree_hash(Path(cfg["aros_sdk"]) / "include")
    key = hashlib.sha256(sdk_include.encode() + b"".join(bytes.fromhex(sha256_file(p)) for p in patches)).hexdigest()[:8]
    out = root / f"aros-glue-{commit[:12]}-{key}"
    if (out / "glue.json").exists():
        return out, json.loads((out / "glue.json").read_text())
    with tempfile.TemporaryDirectory(prefix="glue-", dir=root) as tmp:
        tmp = Path(tmp)
        src = tmp / "src"
        (src / "hosted/rust").mkdir(parents=True)
        for name in GLUE_SOURCES:
            shutil.copy2(checkout / "hosted/rust" / f"{name}.c", src / "hosted/rust" / f"{name}.c")
        for patch in patches:
            run(["patch", "--quiet", "-p1", "-i", patch], cwd=src)
        staged = tmp / "out"
        staged.mkdir()
        for name in GLUE_SOURCES:
            run([cfg["aros_gcc"], f"--sysroot={cfg['aros_sdk']}", "-O2", "-I", checkout / "hosted/rust",
                 "-c", src / "hosted/rust" / f"{name}.c", "-o", staged / f"{name}.o"])
        record = {"pomaros_hosted_commit": commit, "cflags": ["-O2"], "sdk_include_tree_sha256": sdk_include,
                  "patches": {p.name: sha256_file(p) for p in patches},
                  "sources": {f"{n}.c": sha256_file(src / "hosted/rust" / f"{n}.c") for n in GLUE_SOURCES},
                  "objects": {f"{n}.o": sha256_file(staged / f"{n}.o") for n in GLUE_SOURCES}}
        (staged / "glue.json").write_text(json.dumps(record, indent=2) + "\n")
        staged.rename(out)
    return out, record


def cmd_build(args):
    cmd_verify(args)
    cfg = craftconfig.load(["aros_gcc", "aros_ar", "aros_sdk", "rust_aros", "toolchain", "cargo_home", "build_root"])
    v11 = cfg["profile"] == "v11"
    raspi = cfg["profile"] == "aarch64-v1-raspi"
    if cfg["pthread"] not in ("patched", "sdk", "rebuilt"):
        raise Failure(f"pthread is `{cfg['pthread']}`; use patched, rebuilt or sdk")
    if (v11 or raspi) and not cfg.get("aros_src"):
        raise Failure(f"aros_src is required for {cfg['target']} (pthread link library); see `craft.py config`")
    if raspi and not cfg.get("pomaros_hosted"):
        raise Failure("pomaros_hosted is required for aarch64 (C bridge of the std PAL); see `craft.py config`")
    for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS", "CARGO_TARGET_DIR",
                 "CARGO_PROFILE_RELEASE_PANIC"):
        if os.environ.get(name):
            raise Failure(f"clear {name}: it would override the selected native profile or output")
    rustc = run(["rustc", "+" + cfg["toolchain"], "--version"]).stdout.decode().strip()
    if rustc != cfg["rustc_version"]:
        raise Failure(f"toolchain {cfg['toolchain']} reports `{rustc}`, expected `{cfg['rustc_version']}`")
    root = Path(cfg["build_root"])
    root.mkdir(parents=True, exist_ok=True)
    head, status = work_state(WORK)
    files = snapshot(args.worktree)
    src = root / "src"
    sync_tree(files, src)
    # The build copy's lock changes when path patches replace registry crates; keep the
    # original next to it so the manifest can name both.
    lock_original = hashlib.sha256(files["Cargo.lock"][0]).hexdigest()

    rust_aros = Path(cfg["rust_aros"])
    commit = cfg["rust_aros_commit"]
    libc_patches = [LIBC_DIR / n for n in (LIBC_DIR / "series").read_text().split()]
    libc_key = hashlib.sha256(b"".join(bytes.fromhex(sha256_file(p)) for p in libc_patches)).hexdigest()[:8]
    libc = root / f"libc-aros-{commit[:12]}-{libc_key}"
    sysroot = Path(run(["rustc", "+" + cfg["toolchain"], "--print", "sysroot"]).stdout.decode().strip())
    if raspi:
        # std's libc on aarch64 is the common sysroot's adapted copy: no bindings, no C glue (the
        # PAL uses Jonx's bridge instead). Packages depending on libc must get the same crate.
        libc = sysroot / "lib/rustlib/src/rust/library/libc-aros"
        if 'Ok("aarch64") { return; }' not in (libc / "build.rs").read_text():
            raise Failure(f"{libc} is not the common sysroot's aarch64 adaptation")
        if libc_patches:
            raise Failure("patches/libc-aros applies to rust-aros's libc-aros, which aarch64 does not build")
    elif not libc.exists():
        archive = run(["git", "-C", rust_aros, "archive", "--format=tar", commit, "std-groundwork/libc-aros"]).stdout
        with tempfile.TemporaryDirectory(prefix="libc-", dir=root) as tmp:
            with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
                tar.extractall(tmp, filter="data")
            staged = Path(tmp) / "std-groundwork/libc-aros"
            for patch in libc_patches:
                run(["patch", "--quiet", "-p1", "-i", patch], cwd=staged)
            staged.rename(libc)
    if v11:
        template = run(["git", "-C", rust_aros, "show", f"{commit}:x86_64-aros.json.in"]).stdout.decode()
        target_json = root / "x86_64-aros.json"
        target_text = (template.replace("@AROS_GCC@", cfg["aros_gcc"]).replace("@AROS_SDK@", cfg["aros_sdk"])
                       .replace("@AROS_EXTRA_LIBS@", ""))
        target_data = json.loads(target_text)
    elif raspi:
        target_data = json.loads(run(["git", "-C", rust_aros, "show", f"{commit}:profiles/{cfg['profile']}.json"]).stdout)
        glue_dir, glue = build_glue(cfg, root)
        if cfg["pthread"] == "sdk":
            pthread_dir, pthread = None, sdk_pthread_record(cfg)
        else:
            pthread_dir, pthread = build_pthread_raspi(cfg, root)
        # Cargo's ELF section GC drops the generated main and lang_start (CLAUDE.md, "Ordinary Rust
        # Cargo main on native ARM AROS"): the linker is the driver with -Wl,--no-gc-sections after
        # Cargo's arguments, as in rust-aros tools/build-cargo-main.py. Not -Clink-dead-code.
        linker = root / "aarch64-aros-linker"
        linker_text = '#!/bin/sh\nset -e\nexec ' + shlex.quote(cfg["aros_gcc"]) + ' "$@" -Wl,--no-gc-sections\n'
        if not linker.exists() or linker.read_text() != linker_text:
            linker.write_text(linker_text)
        linker.chmod(0o755)
        target_data["linker"] = str(linker)
        target_data["no-default-libraries"] = False
        # The bridge and the corrected pthread members precede the SDK's -lpthread, which then
        # pulls neither replaced member.
        target_data["pre-link-args"] = {"gnu-cc": [f"--sysroot={cfg['aros_sdk']}"] +
                                        [str(glue_dir / f"{n}.o") for n in GLUE_SOURCES] +
                                        ([str(pthread_dir / f"{n}.o") for n in PTHREAD_PATCHED] if pthread_dir else [])}
        target_data["late-link-args"] = {"gnu-cc": ["-lpthread"]}
        target_json = root / f"{cfg['profile']}.json"
    else:
        # rust-aros keeps the mainline profile without machine paths; the linker, the SDK and
        # the pthread library are this build's.
        target_data = json.loads(run(["git", "-C", rust_aros, "show", f"{commit}:profiles/{cfg['profile']}.json"]).stdout)
        target_data["linker"] = cfg["aros_gcc"]
        target_data["pre-link-args"]["gnu-cc"] = [f"--sysroot={cfg['aros_sdk']}"]
        target_data["late-link-args"] = {"gnu-cc": ["-lpthread"]}
        target_json = root / f"{cfg['profile']}.json"
    target_data["pre-link-args"]["gnu-cc"].append("-nix")
    target_text = json.dumps(target_data, indent=2) + "\n"
    if not target_json.exists() or target_json.read_text() != target_text:
        target_json.write_text(target_text)
    crates = cmd_crates(args, quiet=True)
    # The CLI has no GUI dependencies and builds before egui-aros is pinned.
    if "egui-aros" in upstreams():
        egui_aros, egui_aros_record = egui_aros_vendor(cfg, root, src)
    else:
        egui_aros, egui_aros_record = {}, None
    adjusted = adjust_build_copy(cfg, src, libc, egui_aros)
    # Mainline's pthread already frees the slot of a thread detached after it finished (c31d3294).
    if v11 and cfg["pthread"] == "sdk":
        pthread_dir, pthread = None, sdk_pthread_record(cfg)
    elif v11:
        pthread_dir, pthread = build_pthread(cfg, root)
    elif raspi:
        pthread_dir = None  # linked through the target's pre-link arguments (above)
    else:
        pthread_dir, pthread = None, None

    # getrandom has no AROS backend; the getrandom crate patches select getentropy() (patches/crates).
    # The build root first: patched crates and the egui-aros vendor tree live under it, and their
    # panic locations would otherwise carry this machine's paths. A later, more specific prefix
    # (the source copy, libc-aros) wins where both match.
    flags = ["-Cpanic=" + cfg["panic"], "--cfg", 'getrandom_backend="custom"', f"--remap-path-prefix={root}=/build",
             f"--remap-path-prefix={src}=/" + APP,
             f"--remap-path-prefix={cfg['cargo_home']}=/cargo", f"--remap-path-prefix={libc}=/libc-aros",
             f"--remap-path-prefix={sysroot}=/rust"]
    if cfg["panic"] == "unwind":
        flags.append("-Cforce-unwind-tables=yes")
    # The AROS GCC driver searches the SDK's lib/ before any -L given to it, so a patched
    # libpthread.a cannot win the -lpthread lookup. The two patched objects go first on the
    # link line instead (pre-link arguments reach only final links): they define every symbol
    # of the SDK's pthread.o and pthread_create.o, so -lpthread pulls neither. pthread.c also
    # defines the functions of the other members, so whole-archive linking would collide.
    if pthread_dir:
        flags.append("-Zpre-link-args=" + " ".join(str(pthread_dir / f"{n}.o") for n in PTHREAD_PATCHED))
    command = ["cargo", "+" + cfg["toolchain"], "build", "--release", "--target", target_json,
               "-Zbuild-std=std,panic_" + cfg["panic"], "-Zjson-target-spec",
               "--config", "build.rustflags=" + json.dumps(flags),
               "--config", "patch.crates-io.libc.path=" + json.dumps(str(libc))]
    if not args.online:
        command.append("--offline")
    for name, info in crates.items():
        command += ["--config", f"patch.crates-io.{name}.path=" + json.dumps(info["path"]), "--config", f"patch.crates-io.{name}.package=" + json.dumps(info["package"])]
    # The port's own crate patches win where both have one (the same patches today).
    for name, path in egui_aros.items():
        if name not in crates:
            command += ["--config", f"patch.crates-io.{name}.path=" + json.dumps(path)]
    patch_args = [a for i, a in enumerate(command) if isinstance(a, str) and a.startswith("patch.") and command[i - 1] == "--config"]
    pin_libc(cfg, src, libc, [x for a in patch_args for x in ("--config", a)], adjusted)
    if args.tool:
        manifest_dir, tool_binary = TOOLS[args.tool]
        # Built from a copy: the path patches would otherwise rewrite the tool's own lock.
        tool_src = root / "tools" / args.tool
        sync_tree({p.relative_to(REPO / manifest_dir).as_posix(): (p.read_bytes(), os.access(p, os.X_OK))
                   for p in (REPO / manifest_dir).rglob("*") if p.is_file() and "target" not in p.parts}, tool_src)
        command += ["--manifest-path", tool_src / "Cargo.toml"]
        binary_names = [tool_binary]
    else:
        packages = args.package or list(PACKAGES)
        for package in packages:
            if package not in PACKAGES:
                raise Failure(f"unknown package {package}; choose from {sorted(PACKAGES)}")
            command += ["-p", package]
        binary_names = [PACKAGES[p] for p in packages]
    # Release builds: whole-program LTO, which with the stripping at packaging keeps the programs
    # within what apkg accepts for one file (at the same optimisation level).
    if args.lto == "fat":
        command += ["--config", 'profile.release.lto="fat"']
    if args.no_default_features:
        command.append("--no-default-features")
    if args.features:
        command += ["--features", args.features]
    # The release name (packaging/release.json) is what the programs print for --version and in
    # their log, so a tester's report names the exact snapshot.
    release_name = json.loads((REPO / "packaging" / "release.json").read_text())["release"]
    env = dict(os.environ, **{PORT["env_prefix"] + "_BUILD_TARGET": cfg["target"], PORT["env_prefix"] + "_RELEASE": release_name},
               CARGO_HOME=cfg["cargo_home"], CARGO_TARGET_DIR=str(root / "target"),
               AROS_CC=cfg["aros_gcc"], AROS_GCC=cfg["aros_gcc"], AROS_AR=cfg["aros_ar"], AROS_SDK=cfg["aros_sdk"])
    # C code built by cc-rs (ring, for the font store's HTTPS) uses the target's AROS compiler and
    # SDK, never the host's `cc`. cc-rs reads CC_/AR_/CFLAGS_ with the target name's dashes as
    # underscores.
    triple = target_json.stem.replace("-", "_")
    # Static code like the Rust profile's: no PIC (no GOT on AROS), and the large code model on
    # x86_64, whose profiles use it (AROS loads each section wherever memory is free).
    cflags = [f"--sysroot={cfg['aros_sdk']}", "-fno-pic"] + (["-mcmodel=large"] if cfg["target"].startswith("aros-x86_64-") else [])
    env.update({f"CC_{triple}": cfg["aros_gcc"], f"AR_{triple}": cfg["aros_ar"], f"CFLAGS_{triple}": " ".join(cflags)})
    build_id = str(uuid.uuid4())
    out = root / "out" / f"{datetime.datetime.now().strftime('%Y%m%d-%H%M%S')}-{build_id[:8]}"
    out.mkdir(parents=True)
    manifest = {
        "build_id": build_id,
        "tooling": TOOLING,
        "status": "building",
        "date": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "target": cfg["target"],
        "release": release_name,
        "path_mode": "-nix (DOS paths resolved at frontend boundaries)",
        "stack_bytes": 16777216,
        "renderers": {"gui": "egui-aros, chosen at run time (CPU / AUTO / GPU)", "features": args.features or "default"},
        "work_head": head,
        "work_dirty": bool(status),
        "source": "worktree" if args.worktree else "committed HEAD",
        "snapshot_sha256": hashlib.sha256(b"".join(
            k.encode() + b"\0" + hashlib.sha256(v[0]).digest() + (b"x" if v[1] else b"-") for k, v in sorted(files.items()))).hexdigest(),
        "upstreams": upstreams(),
        "series": {n: sha256_file(SERIES_DIR / n) for n in series()},
        "crates": crates,
        "libc_aros": ({"source": "common sysroot (aarch64 adaptation)", "path": str(libc), "tree_sha256": tree_hash(libc)} if raspi else
                      {"rust_aros_commit": commit, "patches": {p.name: sha256_file(p) for p in libc_patches},
                       "tree_sha256": tree_hash(libc)}),
        "target_json_sha256": sha256_file(target_json),
        "pthread": pthread,
        "glue": glue if raspi else None,
        "egui_aros": egui_aros_record,
        "build_copy_adjustments": adjusted,
        "toolchain": {"std_source_sha256": tree_hash(Path(run(["rustc", "+" + cfg["toolchain"], "--print", "sysroot"]).stdout.decode().strip()) / "lib/rustlib/src/rust"),
                      "gcc_sha256": sha256_file(cfg["aros_gcc"]),
                      "sdk_sha256": tree_hash(cfg["aros_sdk"]),
                      "name": cfg["toolchain"], "rustc": rustc,
                      "sysroot": run(["rustc", "+" + cfg["toolchain"], "--print", "sysroot"]).stdout.decode().strip(),
                      "gcc": run([cfg["aros_gcc"], "--version"]).stdout.decode().splitlines()[0],
                      "sdk": cfg["aros_sdk"], "panic": cfg["panic"]},
        "cargo_lock_sha256": lock_original,
        "command": [str(c) for c in command],
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    log = out / "build.log"
    messages = out / "cargo-messages.jsonl"
    # Machine messages on stdout name every compiled unit; they give the exact set of packages
    # linked into the binaries (std's own dependencies included), for the release notices.
    with log.open("wb") as f, messages.open("wb") as m:
        result = subprocess.run([str(c) for c in command] + ["--message-format=json-render-diagnostics"], cwd=src, env=env,
                                stdout=m, stderr=f)
    manifest["returncode"] = result.returncode
    if result.returncode != 0:
        manifest["status"] = "failed"
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        raise Failure(f"build failed; see {log}")
    manifest["cargo_lock_build_sha256"] = sha256_file(src / "Cargo.lock")
    binaries = {}
    release = root / "target" / target_json.stem / "release"
    for name in binary_names:
        path = release / name
        if path.exists():
            shutil.copy2(path, out / name)
            undefined = run([tool(cfg, "nm"), "-u", out / name]).stdout.decode().split()
            binaries[name] = {"sha256": sha256_file(out / name), "bytes": (out / name).stat().st_size,
                              "undefined_symbols": undefined}
            if undefined:
                raise Failure(f"{name} has undefined symbols (collect-aros did not catch them): {undefined[:8]}")
            if cfg["elf"]:
                # ELF64 LSB, OSABI AROS, the target's ABI version, ET_REL, the target's machine; a
                # defined main (section GC once removed it with no link error).
                head_bytes = (out / name).read_bytes()[:20]
                abi, machine = cfg["elf"]
                if (head_bytes[:8] != bytes.fromhex("7f454c460201010f") or head_bytes[8] != abi
                        or int.from_bytes(head_bytes[16:18], "little") != 1 or int.from_bytes(head_bytes[18:20], "little") != machine):
                    raise Failure(f"{name} is not an AROS ELF64 relocatable for {cfg['target']}")
                if not any(line.split()[-2:] == ["T", "main"] for line in run([tool(cfg, "nm"), out / name]).stdout.decode().splitlines()):
                    raise Failure(f"{name} defines no main")
                binaries[name]["elf_checked"] = {"abi_version": abi, "machine": machine, "type": "ET_REL", "main": "defined"}
    if not binaries:
        manifest["status"] = "failed"
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        raise Failure(f"cargo finished but {release} holds none of {binary_names}")
    manifest.update(status="built-not-run", binaries=binaries, linked_packages=linked_packages(messages, target_json.stem))
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"output": str(out), "binaries": binaries, "work_dirty": bool(status)}, indent=2))


def linked_packages(messages, target_dir):
    """Packages compiled for the AROS target (not build scripts or proc-macros, which run on the
    host): package id -> manifest path."""
    found = {}
    for line in messages.read_text().splitlines():
        if not line.startswith("{"):
            continue
        m = json.loads(line)
        if m.get("reason") != "compiler-artifact" or {"custom-build", "proc-macro"} & set(m["target"]["kind"]):
            continue
        if any(f"/{target_dir}/" in name for name in m["filenames"]):
            found[m["package_id"]] = m["manifest_path"]
    return dict(sorted(found.items()))


# Native C test programs: name -> sources relative to the repository.
CTOOLS = {}


def cmd_ctools(args):
    """Builds the native C test programs with the configured compiler and SDK into a new output."""
    cfg = craftconfig.load(["aros_gcc", "aros_sdk", "build_root"])
    out = Path(cfg["build_root"]) / "out" / f"{datetime.datetime.now().strftime('%Y%m%d-%H%M%S')}-ctools-{uuid.uuid4().hex[:8]}"
    out.mkdir(parents=True)
    record = {"status": "built-not-run", "repository_head": git(REPO, "rev-parse", "HEAD"),
              "gcc": run([cfg["aros_gcc"], "--version"]).stdout.decode().splitlines()[0], "sdk": cfg["aros_sdk"], "binaries": {}}
    for name, sources in CTOOLS.items():
        run([cfg["aros_gcc"], f"--sysroot={cfg['aros_sdk']}", "-O2", "-Wall", *[REPO / s for s in sources], "-o", out / name])
        record["binaries"][name] = {"sha256": sha256_file(out / name), "sources": {s: sha256_file(REPO / s) for s in sources}}
    (out / "manifest.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({"output": str(out), **record}, indent=2))


def cmd_host(args):
    """Host cargo in the work copy (tests, checks), with the pinned egui-aros and the crates it
    needs that are not on crates.io as [patch] entries: without them the workspace does not
    resolve, because the AROS shell depends on egui-aros. Runs in a copy of the work copy (its
    files on disk), so the lock entries cargo adds for them never reach the work copy."""
    cfg = craftconfig.load(["cargo_home", "build_root"])
    root = Path(cfg["build_root"])
    root.mkdir(parents=True, exist_ok=True)
    src = root / "host-src"
    sync_tree(snapshot(True), src)
    egui_aros, _ = egui_aros_vendor(cfg, root, src)
    command = ["cargo", "+" + cfg["toolchain"]]
    for name in ("egui-aros", "aros-gui-sys", "aros-startup", "craft-jobs"):
        if name in egui_aros:
            command += ["--config", f"patch.crates-io.{name}.path=" + json.dumps(egui_aros[name])]
    command += args.cargo
    env = dict(os.environ, CARGO_HOME=cfg["cargo_home"], CARGO_TARGET_DIR=str(root / "host-target"))
    result = subprocess.run([str(x) for x in command], cwd=src, env=env)
    if result.returncode:
        raise Failure(f"cargo exited with {result.returncode}")


def cmd_update_check(args):
    import update
    report = update.check(sys.modules[__name__], args.upstream_commit, Path(args.output), Path(args.keep) if args.keep else None)
    print(update.summary(report))


def main(repo, argv, tooling=None):
    """Runs one command for the port in `repo`; returns the process status."""
    try:
        configure(repo, tooling)
    except craftconfig.ConfigError as error:
        print(f"craft.py: {error}", file=sys.stderr)
        return 1
    if argv[:1] == ["package"]:
        import package
        return package.main(repo, argv[1:], tooling)
    parser = argparse.ArgumentParser(prog="craft.py", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("deps", help="fetch the original locked dependencies into the private Cargo home").set_defaults(func=cmd_deps)
    sub.add_parser("config", help="show configuration sources").set_defaults(func=cmd_config)
    sub.add_parser("fetch", help="clone or verify pinned upstreams").set_defaults(func=cmd_fetch)
    sub.add_parser("materialize", help="create the work copy from the series").set_defaults(func=cmd_materialize)
    sub.add_parser("export", help="write work copy commits to the series").set_defaults(func=cmd_export)
    sub.add_parser("verify", help="rebuild the series and compare with the work copy").set_defaults(func=cmd_verify)
    sub.add_parser("crates", help="prepare patched crates").set_defaults(func=cmd_crates)
    sub.add_parser("ctools", help="build the native C test programs (putfile)").set_defaults(func=cmd_ctools)
    h = sub.add_parser("host", help="host cargo in the work copy with the pinned egui-aros patched in")
    h.add_argument("cargo", nargs=argparse.REMAINDER, help="cargo arguments, e.g. test -p NAME")
    h.set_defaults(func=cmd_host)
    b = sub.add_parser("build", help="build AROS binaries")
    b.add_argument("-p", "--package", action="append", default=[], help=", ".join(PACKAGES) + " (default: all)")
    b.add_argument("--features", help="cargo --features")
    b.add_argument("--no-default-features", action="store_true")
    b.add_argument("--lto", choices=["fat"], help="whole-program LTO (release builds)")
    b.add_argument("--worktree", action="store_true", help="build uncommitted files too (recorded as such)")
    b.add_argument("--online", action="store_true", help="allow cargo to download missing crates")
    b.add_argument("--tool", choices=sorted(TOOLS), help="build a test tool from tests/ instead of the app")
    b.set_defaults(func=cmd_build)
    u = sub.add_parser("update-check", help="try the series on another upstream commit, apart from the port; report only")
    u.add_argument("--upstream-commit", required=True, help="full 40-character commit of the app's upstream")
    u.add_argument("--output", required=True, help="where the JSON report is written")
    u.add_argument("--keep", help="keep the trial checkout in this new directory (default: removed)")
    u.set_defaults(func=cmd_update_check)
    args = parser.parse_args(argv)
    try:
        args.func(args)
    except (Failure, craftconfig.ConfigError) as error:
        print(f"craft.py {args.command}: {error}", file=sys.stderr)
        return 1
    return 0
