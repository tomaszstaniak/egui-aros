#!/usr/bin/env python3
"""egui-aros: fetch the pinned external sources, prepare patched crates, and
build this repository's crates and examples for x86_64 ABIv11.

Commands (run `ea.py COMMAND --help`):
  config   show where each setting comes from
  fetch    clone/verify the pinned upstream checkouts (upstream/<id>, sparse)
  crates   prepare the patched crates (crates.io archives or upstream paths)
  build    build AROS binaries from this repository into a fresh output

Nothing here edits upstream/<id>, a shared toolchain, an SDK or the Cargo
registry.
"""
import argparse
import datetime
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import eaconfig  # noqa: E402

REPO = eaconfig.REPO
UPSTREAMS = REPO / "upstreams.json"
CRATES_DIR = REPO / "patches" / "crates"
LIBC_DIR = REPO / "patches" / "libc-aros"
PTHREAD_DIR = REPO / "patches" / "aros-pthread"
# The SDK's libpthread.a objects are reproduced exactly by these flags (checked below).
PTHREAD_CFLAGS = ["-O2", "-mcmodel=large", "-fno-omit-frame-pointer"]
# The members patches/aros-pthread changes; only these are linked from the private build.
PTHREAD_PATCHED = ("pthread", "pthread_create")
# Package -> the AROS binary it produces.
PACKAGES = {"egui-aros-glow-probe": "egui-aros-glow-probe",
            "egui-aros-lifecycle-probe": "egui-aros-lifecycle-probe"}


class Failure(Exception):
    pass


def run(cmd, cwd=None, env=None, capture=True, check=True, input=None):
    result = subprocess.run([str(c) for c in cmd], cwd=cwd, env=env, input=input,
                            stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE if capture else None)
    if check and result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip() if capture else ""
        raise Failure(f"{' '.join(str(c) for c in cmd)} failed ({result.returncode}): {detail}")
    return result


def git(repo, *args, **kw):
    return run(["git", "-C", repo, *args], **kw).stdout.decode().strip()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def tree_hash(root):
    """Hash of every file's relative path, mode bit and contents under root."""
    h = hashlib.sha256()
    for path in sorted(p for p in Path(root).rglob("*") if p.is_file()):
        rel = path.relative_to(root).as_posix()
        h.update(rel.encode() + b"\0" + (b"x" if os.access(path, os.X_OK) else b"-"))
        h.update(bytes.fromhex(sha256_file(path)))
    return h.hexdigest()


def upstreams():
    return json.loads(UPSTREAMS.read_text())["repositories"]


def upstream_dir(rid):
    return REPO / "upstream" / rid


def check_upstream(rid, spec):
    path = upstream_dir(rid)
    if not (path / ".git").exists():
        raise Failure(f"upstream/{rid} is missing; run `ea.py fetch`")
    head = git(path, "rev-parse", "HEAD")
    if head != spec["commit"]:
        raise Failure(f"upstream/{rid} is at {head}, the pin is {spec['commit']}")
    if git(path, "status", "--porcelain"):
        raise Failure(f"upstream/{rid} has local changes; it must stay clean")
    return path


def cmd_config(args):
    print(eaconfig.describe())


def cmd_fetch(args):
    for rid, spec in upstreams().items():
        path = upstream_dir(rid)
        if not path.exists():
            path.parent.mkdir(parents=True, exist_ok=True)
            run(["git", "clone", "--quiet", "--filter=blob:none", "--no-checkout", spec["url"], path], capture=False)
            run(["git", "-C", path, "sparse-checkout", "set", "--no-cone", *[f"/{p}/" for p in spec["paths"]]])
            run(["git", "-C", path, "checkout", "--quiet", spec["commit"]])
        check_upstream(rid, spec)
        print(f"upstream/{rid}: {spec['commit']} clean ({', '.join(spec['paths'])})")


def crate_specs():
    return json.loads((CRATES_DIR / "crates.json").read_text())["crates"]


def series_patches(name):
    """The patch files of a crate's series: `<id>:<path>` names a file of a pinned upstream
    (applied unchanged), anything else a file beside the series."""
    out = []
    series = CRATES_DIR / name / "series"
    for entry in series.read_text().split() if series.exists() else []:
        rid, sep, rel = entry.partition(":")
        if sep:
            out.append(check_upstream(rid, upstreams()[rid]) / rel)
        else:
            out.append(CRATES_DIR / name / entry)
    return out


def crate_source(spec, cfg):
    """A temporary unpacked copy of the crate before our patches, and its origin."""
    name = f"{spec['name']}-{spec['version']}"
    if "upstream" in spec:
        rid, rel = spec["upstream"].split(":", 1)
        base = check_upstream(rid, upstreams()[rid])
        data = run(["git", "-C", base, "archive", "--format=tar", "HEAD", rel]).stdout
        return data, rel, {"upstream": rid, "commit": upstreams()[rid]["commit"], "path": rel}
    archives = list(Path(cfg["cargo_home"]).glob(f"registry/cache/*/{name}.crate"))
    if len(archives) != 1:
        raise Failure(f"{name}.crate is not in {cfg['cargo_home']}; run `ea.py build --online` once to fetch dependencies")
    if sha256_file(archives[0]) != spec["checksum"]:
        raise Failure(f"{archives[0]} does not match the Cargo.lock checksum")
    return archives[0].read_bytes(), name, {"registry_checksum": spec["checksum"]}


def cmd_crates(args, quiet=False):
    cfg = eaconfig.load(["cargo_home", "build_root"])
    out = {}
    import tomllib
    locked = {(p["name"], p["version"]) for p in tomllib.loads((REPO / "Cargo.lock").read_text())["package"]}
    for spec in crate_specs():
        name = f"{spec['name']}-{spec['version']}"
        # A registry crate the lock does not use needs no copy (cargo would only warn about it).
        if "upstream" not in spec and (spec["name"], spec["version"]) not in locked:
            if not quiet:
                print(f"{name}: not in Cargo.lock, skipped")
            continue
        patches = series_patches(name)
        key = hashlib.sha256(b"".join(bytes.fromhex(sha256_file(p)) for p in patches)).hexdigest()[:8]
        dest = Path(cfg["build_root"]) / "crates" / f"{name}-{key}"
        data, inner, origin = crate_source(spec, cfg)
        if not dest.exists():
            dest.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix="crate-", dir=dest.parent) as tmp:
                with tarfile.open(fileobj=io.BytesIO(data)) as tar:
                    tar.extractall(tmp, filter="data")
                staged = Path(tmp) / inner
                for patch in patches:
                    run(["patch", "--quiet", "-p1", "-i", patch], cwd=staged)
                staged.rename(dest)
        got = tree_hash(dest)
        if spec.get("tree_sha256") and got != spec["tree_sha256"]:
            raise Failure(f"{dest} tree {got} differs from the reviewed {spec['tree_sha256']}; remove it to rebuild")
        out[spec["name"]] = {"path": str(dest), "version": spec["version"], "origin": origin,
                             "patches": {p.name: sha256_file(p) for p in patches}, "tree_sha256": got}
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
        tmp = path.with_name(path.name + ".ea-tmp")
        tmp.write_bytes(data)
        tmp.chmod(0o755 if executable else 0o644)
        tmp.replace(path)
    wanted = set(files)
    for path in sorted(dest.rglob("*"), reverse=True):
        rel = path.relative_to(dest).as_posix()
        if rel.startswith("target/"):
            continue
        if path.is_file() and rel not in wanted:
            path.unlink()
        elif path.is_dir() and not any(path.iterdir()):
            path.rmdir()


def snapshot(worktree):
    """The repository's files: committed HEAD, or (worktree) the files on disk Git would track."""
    files = {}
    if worktree:
        names = run(["git", "-C", REPO, "ls-files", "-z", "--cached", "--others", "--exclude-standard"]).stdout.split(b"\0")
        for name in filter(None, names):
            path = REPO / name.decode()
            if path.is_file():
                files[name.decode()] = (path.read_bytes(), os.access(path, os.X_OK))
        return files
    data = run(["git", "-C", REPO, "archive", "--format=tar", "HEAD"]).stdout
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        for member in tar.getmembers():
            if member.isfile():
                files[member.name] = (tar.extractfile(member).read(), bool(member.mode & 0o111))
    return files


def objdump_key(cfg, obj):
    tool = cfg["aros_gcc"].replace("-gcc", "-objdump")
    text = run([tool, "-d", "-r", "--no-show-raw-insn", obj]).stdout.decode().split("\n", 4)[-1]
    return hashlib.sha256(text.encode()).hexdigest()


def build_pthread(cfg, root):
    """libpthread.a's two patched objects from deadwood2/AROS compiler/pthread with
    patches/aros-pthread applied. The unpatched tree is first checked to rebuild the SDK's
    archive object for object, so the patch is all that differs."""
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


def libc_aros(cfg, root):
    rust_aros = Path(cfg["rust_aros"])
    commit = cfg["rust_aros_commit"]
    patches = [LIBC_DIR / n for n in (LIBC_DIR / "series").read_text().split()]
    key = hashlib.sha256(b"".join(bytes.fromhex(sha256_file(p)) for p in patches)).hexdigest()[:8]
    libc = root / f"libc-aros-{commit[:12]}-{key}"
    if not libc.exists():
        archive = run(["git", "-C", rust_aros, "archive", "--format=tar", commit, "std-groundwork/libc-aros"]).stdout
        with tempfile.TemporaryDirectory(prefix="libc-", dir=root) as tmp:
            with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
                tar.extractall(tmp, filter="data")
            staged = Path(tmp) / "std-groundwork/libc-aros"
            for patch in patches:
                run(["patch", "--quiet", "-p1", "-i", patch], cwd=staged)
            staged.rename(libc)
    return libc, {"rust_aros_commit": commit, "patches": {p.name: sha256_file(p) for p in patches},
                  "tree_sha256": tree_hash(libc)}


def linked_packages(messages):
    """Packages compiled for the AROS target (not build scripts or proc-macros, which run on the
    host): package id -> manifest path."""
    found = {}
    for line in messages.read_text().splitlines():
        if not line.startswith("{"):
            continue
        m = json.loads(line)
        if m.get("reason") != "compiler-artifact" or {"custom-build", "proc-macro"} & set(m["target"]["kind"]):
            continue
        if any("/x86_64-aros/" in name for name in m["filenames"]):
            found[m["package_id"]] = m["manifest_path"]
    return dict(sorted(found.items()))


def cmd_build(args):
    cfg = eaconfig.load(["aros_gcc", "aros_ar", "aros_sdk", "aros_src", "rust_aros", "toolchain", "cargo_home", "build_root"])
    for name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS", "CARGO_TARGET_DIR",
                 "CARGO_PROFILE_RELEASE_PANIC"):
        if os.environ.get(name):
            raise Failure(f"clear {name}: it would override the selected native profile or output")
    rustc = run(["rustc", "+" + cfg["toolchain"], "--version"]).stdout.decode().strip()
    if rustc != cfg["rustc_version"]:
        raise Failure(f"toolchain {cfg['toolchain']} reports `{rustc}`, expected `{cfg['rustc_version']}`")
    sysroot = run(["rustc", "+" + cfg["toolchain"], "--print", "sysroot"]).stdout.decode().strip()
    root = Path(cfg["build_root"])
    root.mkdir(parents=True, exist_ok=True)
    head = git(REPO, "rev-parse", "HEAD")
    dirty = bool(git(REPO, "status", "--porcelain"))
    files = snapshot(args.worktree)
    src = root / "src"
    sync_tree(files, src)

    libc, libc_record = libc_aros(cfg, root)
    template = run(["git", "-C", cfg["rust_aros"], "show", f"{cfg['rust_aros_commit']}:x86_64-aros.json.in"]).stdout.decode()
    target_json = root / "x86_64-aros.json"
    target_text = (template.replace("@AROS_GCC@", cfg["aros_gcc"]).replace("@AROS_SDK@", cfg["aros_sdk"])
                   .replace("@AROS_EXTRA_LIBS@", ""))
    if not target_json.exists() or target_json.read_text() != target_text:
        target_json.write_text(target_text)
    crates = {} if args.online and not (Path(cfg["cargo_home"]) / "registry").exists() else cmd_crates(args, quiet=True)
    pthread_dir, pthread = build_pthread(cfg, root)

    flags = ["-Cpanic=" + cfg["panic"], "--cfg", 'getrandom_backend="custom"', f"--remap-path-prefix={src}=/egui-aros",
             f"--remap-path-prefix={cfg['cargo_home']}=/cargo", f"--remap-path-prefix={libc}=/libc-aros",
             f"--remap-path-prefix={root / 'crates'}=/crates", f"--remap-path-prefix={sysroot}=/rust"]
    if cfg["panic"] == "unwind":
        flags.append("-Cforce-unwind-tables=yes")
    # The AROS GCC driver searches the SDK's lib/ before any -L given to it, so a patched
    # libpthread.a cannot win the -lpthread lookup; the two patched objects go first on the
    # link line instead (see the WordCraft and PhotoCraft ports).
    flags.append("-Zpre-link-args=" + " ".join(str(pthread_dir / f"{n}.o") for n in PTHREAD_PATCHED))
    command = ["cargo", "+" + cfg["toolchain"], "build", "--release", "--target", target_json,
               "-Zbuild-std=std,panic_" + cfg["panic"], "-Zjson-target-spec",
               "--config", "build.rustflags=" + json.dumps(flags),
               "--config", "patch.crates-io.libc.path=" + json.dumps(str(libc))]
    if not args.online:
        command.append("--offline")
    for name, info in crates.items():
        command += ["--config", f"patch.crates-io.{name}.path=" + json.dumps(info["path"])]
    packages = args.package or list(PACKAGES)
    for package in packages:
        if package not in PACKAGES:
            raise Failure(f"unknown package {package}; choose from {sorted(PACKAGES)}")
        command += ["-p", package]
    if args.no_default_features:
        command.append("--no-default-features")
    if args.features:
        command += ["--features", args.features]
    env = dict(os.environ, CARGO_HOME=cfg["cargo_home"], CARGO_TARGET_DIR=str(root / "target"),
               AROS_CC=cfg["aros_gcc"], AROS_GCC=cfg["aros_gcc"], AROS_AR=cfg["aros_ar"], AROS_SDK=cfg["aros_sdk"],
               CFLAGS_x86_64_aros=f"--sysroot={cfg['aros_sdk']}")
    build_id = str(uuid.uuid4())
    out = root / "out" / f"{datetime.datetime.now().strftime('%Y%m%d-%H%M%S')}-{build_id[:8]}"
    out.mkdir(parents=True)
    lock = src / "Cargo.lock"
    manifest = {
        "build_id": build_id,
        "status": "building",
        "date": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "abi": "x86_64 ABIv11",
        "repository_head": head,
        "repository_dirty": dirty,
        "source": "worktree" if args.worktree else "committed HEAD",
        "snapshot_sha256": hashlib.sha256(b"".join(
            k.encode() + b"\0" + hashlib.sha256(v[0]).digest() + (b"x" if v[1] else b"-") for k, v in sorted(files.items()))).hexdigest(),
        "upstreams": upstreams(),
        "crates": crates,
        "libc_aros": libc_record,
        "target_json_sha256": sha256_file(target_json),
        "pthread": pthread,
        "toolchain": {"name": cfg["toolchain"], "rustc": rustc, "sysroot": sysroot,
                      "gcc": run([cfg["aros_gcc"], "--version"]).stdout.decode().splitlines()[0],
                      "gcc_path": cfg["aros_gcc"], "ar_path": cfg["aros_ar"], "sdk": cfg["aros_sdk"],
                      "panic": cfg["panic"]},
        "cargo_lock_sha256": sha256_file(lock) if lock.exists() else None,
        "command": [str(c) for c in command],
        "env": {k: env[k] for k in ("AROS_CC", "AROS_AR", "AROS_SDK", "CFLAGS_x86_64_aros")},
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    log = out / "build.log"
    messages = out / "cargo-messages.jsonl"
    with log.open("wb") as f, messages.open("wb") as m:
        result = subprocess.run([str(c) for c in command] + ["--message-format=json-render-diagnostics"], cwd=src, env=env,
                                stdout=m, stderr=f)
    manifest["returncode"] = result.returncode
    if result.returncode != 0:
        manifest["status"] = "failed"
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        raise Failure(f"build failed; see {log}")
    binaries = {}
    release = root / "target" / "x86_64-aros" / "release"
    for package in packages:
        name = PACKAGES[package]
        path = release / name
        if path.exists():
            shutil.copy2(path, out / name)
            undefined = run([cfg["aros_gcc"].replace("-gcc", "-nm"), "-u", out / name]).stdout.decode().split()
            binaries[name] = {"sha256": sha256_file(out / name), "bytes": (out / name).stat().st_size,
                              "undefined_symbols": undefined}
            if undefined:
                raise Failure(f"{name} has undefined symbols (collect-aros did not catch them): {undefined[:8]}")
    manifest.update(status="built-not-run", binaries=binaries, linked_packages=linked_packages(messages),
                    cargo_lock_build_sha256=sha256_file(lock) if lock.exists() else None)
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"output": str(out), "binaries": binaries, "repository_dirty": dirty}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("config", help="show configuration sources").set_defaults(func=cmd_config)
    sub.add_parser("fetch", help="clone or verify pinned upstreams").set_defaults(func=cmd_fetch)
    sub.add_parser("crates", help="prepare patched crates").set_defaults(func=cmd_crates)
    b = sub.add_parser("build", help="build AROS binaries")
    b.add_argument("-p", "--package", action="append", default=[], help=f"one of {sorted(PACKAGES)} (default: all)")
    b.add_argument("--features", help="cargo --features")
    b.add_argument("--no-default-features", action="store_true")
    b.add_argument("--worktree", action="store_true", help="build uncommitted files too (recorded as such)")
    b.add_argument("--online", action="store_true", help="allow cargo to download missing crates")
    b.set_defaults(func=cmd_build)
    args = parser.parse_args()
    try:
        args.func(args)
    except (Failure, eaconfig.ConfigError) as error:
        parser.exit(1, f"ea.py {args.command}: {error}\n")


if __name__ == "__main__":
    main()
