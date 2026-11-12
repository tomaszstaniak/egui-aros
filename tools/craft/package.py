#!/usr/bin/env python3
"""Package `craft.py build` outputs as the release archives described in packaging/release.json.

    craft.py package binary BUILD_OUTPUT [--dist DIR]
    craft.py package source BUILD_OUTPUT [BUILD_OUTPUT ...] [--dist DIR]

`binary` writes <App>-<release>[-rN]-<target>.zip for the build's target (read from its
manifest). `source` writes <App>-<release>[-rN]-source.zip: this repository at HEAD (pins,
patches, scripts, recipe) plus the pinned inputs every named build used. Both append to
DIR/SHA256SUMS (default DIR: dist/ of this repository, flat; the target is in the file name).
The app's names come from scripts/port.json, as in craft.py. `-rN` is the packaging revision
(`revision` in release.json, arospkg's revision of the same program version): a correction of
the packages that keeps the programs as they are, or, with `"revision_programs": "rebuilt"`, new
programs from the same upstream version (changes of the port); those print `<release>-rN`.

The configuration of a build's target is the local file (<PREFIX>_LOCAL, local.json,
local.*.json) whose `target` is that target; local.json without a `target` is the default
target, as in craftconfig. Exactly one must match.

Refuses to replace a file that is already there or listed in SHA256SUMS, a build that is not
from a clean, committed work copy or made under another release name, a build with another
LTO/panic/feature profile than the target's in release.json, binaries whose bytes differ from
the build manifest or that have undefined symbols, a repository whose patch series is not
the one the build used, and a catalogue description whose system requirements do not match
the programs: every C runtime library the programs open at start (ABIv11: crt, stdlib, m;
mainline ABIv1: stdc, posixc, stdcio) must be required, and nothing may be required that no
program names.

The binary archive unpacks to one drawer and its icon (WordCraft shown):

    WordCraft.info                drawer icon
    WordCraft/WordCraft           the editor, stripped (--strip-unneeded --remove-section .comment)
    WordCraft/WordCraft.info      its tool icon: stack and ToolTypes from release.json
    WordCraft/wordcraft-cli       command-line program, stripped the same way
    WordCraft/ReadMe.txt          installation and use (packaging/ReadMe.txt for the target)
    WordCraft/BUILD.txt           what the binaries were built from, and their hashes
    WordCraft/Licenses/...        the app's licences and notices, AROS-NOTICE.txt,
                                  AROS-LICENSE.txt, THIRD-PARTY.txt (every crate linked in)
                                  and the Rust standard library's licence texts

Entries are sorted, dated with the repository's HEAD commit time and carry Unix modes (0755 for
the programs), so the same inputs give the same bytes. The unstripped binaries stay where the
build put them (build/<target>/out/<id>/); BUILD.txt records the hashes of both forms.
"""
import argparse
import calendar
import contextlib
import hashlib
import io
import json
import lzma
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import icons  # noqa: E402
import craftconfig  # noqa: E402

LICENSE_NAMES = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|COPYRIGHT|UNLICENSE)", re.I)
# Set by configure() for the port being packaged (see craftconfig.configure).
REPO = PORT = APP = ID = LOCAL_VAR = WORK = SERIES = PACKAGING = None
BINARIES = ()
# How the tools were obtained (the port's bootstrap passes it); bundled with the source archive.
TOOLING = None


def configure(repo, tooling=None):
    global REPO, PORT, APP, ID, LOCAL_VAR, WORK, SERIES, PACKAGING, BINARIES, TOOLING, PACK_ADDED
    craftconfig.configure(repo)
    REPO = craftconfig.REPO
    PORT = craftconfig.PORT
    APP = PORT["name"]
    ID = PORT["id"]
    LOCAL_VAR = PORT["env_prefix"] + "_LOCAL"
    WORK = REPO / "work" / ID
    SERIES = REPO / "patches" / ID
    PACKAGING = REPO / "packaging"
    BINARIES = tuple(PORT["packages"].values())
    PACK_ADDED = {".arospkg/manifest.toml", f"{APP}/application.json"}
    TOOLING = tooling


# apkg 0.6 accepts one file of up to 128 MiB unpacked.
MEMBER_LIMIT = 128 << 20
# Classic FFS volumes take names of up to 30 characters.
NAME_LIMIT = 30


class Failure(Exception):
    pass


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def git(*args, cwd=None):
    return subprocess.run(["git", "-C", str(cwd or REPO), *args], check=True, capture_output=True).stdout


def run(cmd, **kw):
    return subprocess.run([str(c) for c in cmd], check=True, capture_output=True, **kw).stdout


@contextlib.contextmanager
def target_env(path):
    old = os.environ.get(LOCAL_VAR)
    os.environ[LOCAL_VAR] = str(path)
    try:
        yield
    finally:
        if old is None:
            os.environ.pop(LOCAL_VAR)
        else:
            os.environ[LOCAL_VAR] = old


def config_for(target):
    """The resolved configuration of `target`, from the one local file that names it."""
    candidates = []
    if os.environ.get(LOCAL_VAR):
        candidates.append(Path(os.environ[LOCAL_VAR]))
    candidates += [REPO / "local.json", *sorted(REPO.glob("local.*.json"))]
    default = craftconfig.SETTINGS["target"][1]
    matches = []
    for path in dict.fromkeys(p.resolve() for p in candidates):
        if not path.is_file() or path.name.endswith(".example.json"):
            continue
        if json.loads(path.read_text()).get("target", default) == target:
            matches.append(path)
    if len(matches) != 1:
        raise Failure(f"need exactly one local file for {target}, found {[str(m) for m in matches]}")
    with target_env(matches[0]):
        cfg = craftconfig.load(["aros_gcc", "aros_strip", "aros_sdk", "rust_aros", "toolchain", "cargo_home"])
    if cfg["target"] != target:
        raise Failure(f"{matches[0]} resolves to {cfg['target']}, not {target} (environment overrides?)")
    return cfg


def tool(cfg, name):
    driver = cfg["aros_gcc"]
    for suffix in ("-gcc", "-clang"):
        if driver.endswith(suffix):
            return driver[: -len(suffix)] + "-" + name
    raise Failure(f"cannot derive {name} from {driver}")


def head_clean():
    if git("status", "--porcelain").strip():
        raise Failure("commit this repository first: archives record and contain HEAD")
    return git("rev-parse", "HEAD").decode().strip()


def check_build(build, release):
    manifest = json.loads((build / "manifest.json").read_text())
    target = manifest.get("target")
    spec = release["targets"].get(target)
    if not spec:
        raise Failure(f"{build}: target {target!r} has no section in release.json")
    if manifest.get("status") not in ("built-not-run", "accepted"):
        raise Failure(f"{build}: build status is {manifest.get('status')!r}, not a finished build")
    if manifest.get("release") != release["release"]:
        raise Failure(f"{build}: built as release {manifest.get('release')!r}, release.json is {release['release']!r}")
    # Programs rebuilt for a revision print `<release>-rN`: a build made under another name (the
    # first packages' programs, for one) is not this revision's.
    if release.get("revision_programs") == "rebuilt":
        shown = f"{release['release']}-r{release.get('revision')}"
        if manifest.get("release_shown") != shown:
            raise Failure(f"{build}: its programs print {manifest.get('release_shown', manifest.get('release'))!r}, this revision's print {shown!r}; rebuild")
    if manifest["work_dirty"] or manifest["source"] != "committed HEAD":
        raise Failure(f"{build}: built from a dirty or uncommitted work copy")
    command = manifest["command"]
    profile = spec["build"]
    lto = 'profile.release.lto="fat"' in command
    if lto != (profile["lto"] == "fat"):
        raise Failure(f"{build}: LTO {'fat' if lto else 'off'}, release.json wants {profile['lto'] or 'off'}")
    if manifest["toolchain"]["panic"] != profile["panic"]:
        raise Failure(f"{build}: panic={manifest['toolchain']['panic']}, release.json wants {profile['panic']}")
    features = [command[i + 1] for i, c in enumerate(command) if c == "--features"]
    if sorted(features) != sorted(profile["features"]) or "--no-default-features" in command:
        raise Failure(f"{build}: features {features} are not the release profile {profile['features']}")
    if sorted(manifest.get("binaries", {})) != sorted(BINARIES):
        raise Failure(f"{build}: holds {sorted(manifest.get('binaries', {}))}, need both {list(BINARIES)}")
    if "linked_packages" not in manifest:
        raise Failure(f"{build}: the manifest has no linked_packages (rebuild with the current craft.py)")
    for name, info in manifest["binaries"].items():
        if sha256((build / name).read_bytes()) != info["sha256"]:
            raise Failure(f"{build / name}: bytes differ from the build manifest")
    series_dir = SERIES
    current = {n: sha256((series_dir / n).read_bytes()) for n in (series_dir / "series").read_text().split()}
    if current != manifest["series"]:
        raise Failure(f"patches/{ID} is not the series this build used")
    if manifest["upstreams"] != json.loads((REPO / "upstreams.json").read_text())["repositories"]:
        raise Failure("upstreams.json differs from the pins this build used")
    version = tomllib.loads((WORK / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if version != release["version"]:
        raise Failure(f"work copy version {version} is not the release version {release['version']}")
    if git("rev-parse", "HEAD", cwd=WORK).decode().strip() != manifest["work_head"]:
        raise Failure(f"work copy is not at the build's commit {manifest['work_head']} (notices are read from it)")
    return manifest, spec


def stripped(cfg, data, name):
    """The program with unneeded symbols and .comment removed: the form verified to load on AROS
    (a full strip loses the relocations AROS applies at load time)."""
    with tempfile.TemporaryDirectory() as tmp:
        src, dst = Path(tmp) / "in", Path(tmp) / "out"
        src.write_bytes(data)
        run([cfg["aros_strip"], "--strip-unneeded", "--remove-section", ".comment", "-o", dst, src])
        undefined = run([tool(cfg, "nm"), "-u", dst]).decode().split()
        if undefined:
            raise Failure(f"{name} has undefined symbols after stripping: {undefined[:8]}")
        out = dst.read_bytes()
    if len(out) > MEMBER_LIMIT:
        raise Failure(f"{name} is {len(out)} bytes after stripping, over apkg's 128 MiB limit for one file")
    return out


def third_party(manifest, build_root):
    """Every package linked into the binaries, except the application's own (covered by its licence),
    with its licence expression and licence files."""
    parts = [f"Third-party Rust packages linked into {' and '.join(BINARIES)}",
             "=" * 68, "",
             f"Target: {manifest['target']}.",
             "Built with the Rust standard library and these packages. Each entry gives the",
             "package, its version, its declared license and the license files it ships.", ""]
    own = [str(WORK.resolve()), str((build_root / "src").resolve())]
    entries = []
    for package_id, manifest_path in manifest["linked_packages"].items():
        crate_dir = Path(manifest_path).parent
        meta = tomllib.loads(Path(manifest_path).read_text()).get("package", {})
        name, version = meta.get("name", "?"), meta.get("version", "?")
        if isinstance(version, dict) or isinstance(meta.get("license"), dict):
            root = crate_dir
            while root != root.parent and not ((root / "Cargo.toml").exists() and "workspace" in tomllib.loads((root / "Cargo.toml").read_text())):
                root = root.parent
            ws = tomllib.loads((root / "Cargo.toml").read_text()).get("workspace", {}).get("package", {})
            version = ws.get("version", "?") if isinstance(version, dict) else version
            meta = {**meta, "license": ws.get("license", meta.get("license"))}
        if any(str(crate_dir.resolve()).startswith(o) for o in own):
            continue
        license_expr = meta.get("license") or ("see " + meta["license-file"] if meta.get("license-file") else "not declared")
        files = sorted(p for p in crate_dir.iterdir() if p.is_file() and LICENSE_NAMES.match(p.name))
        if not files and "/lib/rustlib/src/rust/" in str(crate_dir):
            # The standard library's crates carry no licence files; the Rust tree's root has them.
            rust = Path(str(crate_dir).split("/lib/rustlib/src/rust/")[0] + "/lib/rustlib/src/rust")
            files = sorted(p for p in rust.iterdir() if p.is_file() and LICENSE_NAMES.match(p.name))
            license_expr = license_expr if license_expr != "not declared" else "MIT OR Apache-2.0 (the Rust project)"
        entries.append((name, version, license_expr, files))
    entries.sort(key=lambda e: (e[0], e[1]))
    for name, version, license_expr, files in entries:
        parts += ["-" * 68, f"{name} {version}", f"License: {license_expr}", ""]
        for f in files:
            parts += [f"--- {f.name} ---", f.read_text(errors="replace").rstrip(), ""]
        if not files:
            parts += ["(the package ships no separate license file)", ""]
    libc_id = next((k for k in manifest["linked_packages"] if "#libc@" in k), None)
    if libc_id:
        vendored = Path(manifest["linked_packages"][libc_id]).parent
        for rel in ["csrc/vendor/jitterentropy/LICENSE.bsd", "csrc/vendor/arospkg-entropy/LICENSE"]:
            path = vendored / rel
            if path.is_file():
                parts += ["-" * 68, f"libc-aros: {rel}", "", path.read_text(errors="replace").rstrip(), ""]
    return "\n".join(parts) + "\n", len(entries)


def rust_docs(cfg, manifest):
    """The Rust toolchain's licence notices for its standard library (share/doc/rust of the rustc
    the build used, which must report the build's version)."""
    wrapper = Path(run(["rustup", "which", "rustc", "--toolchain", cfg["toolchain"]], text=True).strip())
    # A private toolchain's rustc is a script that runs the host nightly's rustc on its sysroot.
    found = re.search(r"exec (\S+/bin/rustc)", wrapper.read_text(errors="replace")) if wrapper.stat().st_size < 4096 else None
    rustc = Path(found.group(1)) if found else wrapper
    version = run([rustc, "-V"], text=True).strip()
    if version != manifest["toolchain"]["rustc"]:
        raise Failure(f"{rustc} reports {version!r}, not the build's {manifest['toolchain']['rustc']!r}")
    doc = rustc.parent.parent / "share" / "doc" / "rust"
    out = {"Rust/COPYRIGHT-library.html": (doc / "COPYRIGHT-library.html").read_bytes()}
    for f in sorted((doc / "licenses").iterdir()):
        out[f"Rust/licenses/{f.name}"] = f.read_bytes()
    return out


def build_txt(release, spec, manifest, repo_head, packaged):
    up = manifest["upstreams"]
    tc = manifest["toolchain"]
    lines = [f"{APP} {release['release']} for AROS", "",
             f"Target:        {manifest['target']}",
             f"System:        {spec['system']}",
             f"Version:       {release['upstream_version']}",
             *([f"Package:       revision {release['revision']} of the {release['release']} packages "
                + ("(the programs were rebuilt: see the release notes)" if release.get("revision_programs") == "rebuilt"
                   else "(the programs are unchanged)")]
               if release.get("revision") else []),
             f"Build id:      {manifest['build_id']}",
             f"Built:         {manifest['date']}",
             "",
             "Sources",
             f"  {APP:<19} {up[ID]["url"]} at {up[ID]["commit"]}",
             f"  Port repository     commit {repo_head} (source archive {release['source_archive']})",
             f"  Port work copy      commit {manifest['work_head']}",
             f"  Patch series        patches/{ID}/series (SHA-256):"]
    lines += [f"    {h}  {n}" for n, h in manifest["series"].items()]
    lines += [f"  egui-aros           {up['egui-aros']['url']} at {up['egui-aros']['commit']}",
              f"  egui (storytold)    {up['storytold-egui']['url']} at {up['storytold-egui']['commit']}",
              f"  winit (storytold)   {up['storytold-winit']['url']} at {up['storytold-winit']['commit']}"]
    libc = manifest["libc_aros"]
    if "rust_aros_commit" in libc:
        lines.append(f"  rust-aros           libc-aros at {libc['rust_aros_commit']}")
    else:
        lines.append(f"  libc-aros           {libc['source']}, tree sha256 {libc['tree_sha256']}")
    pthread = manifest.get("pthread")
    if pthread and "aros_src_commit" in pthread:
        lines.append(f"  AROS pthread        compiler/pthread at {pthread['aros_src_commit']}, patches/aros-pthread applied")
    else:
        lines.append("  AROS pthread        the SDK's libpthread.a, unchanged")
    if manifest.get("glue"):
        glue_patches = ", ".join(sorted(manifest["glue"].get("patches", {}))) or "unchanged"
        lines.append(f"  Rust std C bridge   pomaros-hosted hosted/rust at {manifest['glue']['pomaros_hosted_commit']}; patches/aros-glue: {glue_patches}")
    lines += ["  Patched crates:"]
    lines += [f"    {c['package']} {c['version']}  tree {c['tree_sha256']}" for c in manifest["crates"].values()]
    lines += ["",
              "Toolchain",
              f"  Rust        {tc['rustc']} ({tc['name']})",
              f"              std source tree sha256 {tc['std_source_sha256']}",
              f"  C compiler  {tc['gcc']}",
              f"  SDK         {spec['sdk']}",
              f"              tree sha256 {tc['sdk_sha256']}",
              f"  Profile     release, panic={tc['panic']}, LTO {spec['build']['lto'] or 'off'}, linked with -nix",
              "",
              "Programs (SHA-256)",
              "  as built (unstripped):"]
    lines += [f"    {info['sha256']}  {name}  ({info['bytes']} bytes)" for name, info in sorted(manifest["binaries"].items())]
    lines += ["  as packaged (strip --strip-unneeded --remove-section .comment):"]
    lines += [f"    {sha256(data)}  {name}  ({len(data)} bytes)" for name, data in sorted(packaged.items())]
    lines += ["", "Rebuild: docs/development.md in the source archive."]
    return "\n".join(lines) + "\n"


def readme(release, spec):
    text = (PACKAGING / "ReadMe.txt").read_text()
    for key, value in (("@VERSION@", release["release"]), ("@UPSTREAM_VERSION@", release["upstream_version"]),
                       ("@SYSTEM@", spec["system"]),
                       ("@LIMITATIONS@", "\n".join(release["limitations"] + spec["limitations"]))):
        text = text.replace(key, value)
    leftover = re.findall(r"@[A-Z_]+@", text)
    if leftover:
        raise Failure(f"packaging/ReadMe.txt has unknown placeholders {leftover}")
    return text.encode()


def zip_entry(z, name, data, mode, date):
    info = zipfile.ZipInfo(name, date_time=date)
    info.create_system = 3
    info.external_attr = (mode & 0xFFFF) << 16
    info.compress_type = zipfile.ZIP_DEFLATED
    z.writestr(info, data, compresslevel=9)


# arospkg's catalogue key for each target. The catalogue has no platform field: the Pi entry's
# notes say that it is the native Raspberry Pi build, not hosted Macaros.
CATALOGUE = {"aros-x86_64-abiv11-pc": ("x86_64", "v11"), "aros-x86_64-abiv1-pc": ("x86_64", "v1"),
             "aros-aarch64-abiv1-raspi": ("aarch64", "v1")}
# Written by apkg-pack: the description embedded for `apkg-pack submit`, and the launch record
# for `apkg run` in an unpacked drawer.
# What apkg-pack adds to the archive; set by configure() (the app's drawer name is in it).
PACK_ADDED = set()
# The C runtime libraries an AROS program opens at start when it is linked against the C library
# of its ABI. They differ between the ABIs and none exists on the other system, so a description
# that names the wrong set makes apkg refuse the package on the system it was built for.
C_RUNTIME = {"crt.library", "stdlib.library", "m.library",         # ABIv11 (AROS One)
             "stdc.library", "posixc.library", "stdcio.library"}   # mainline ABIv1
LIBRARY_NAME = re.compile(rb"[A-Za-z0-9_.-]+\.library")


def library_names(programs):
    """Every library name that appears in the programs' bytes (the auto-open tables and the
    names the code opens itself)."""
    return {m.decode() for data in programs.values() for m in LIBRARY_NAME.findall(data)}


def check_requirements(meta, programs):
    """The catalogue description must require every C runtime library the programs name and
    nothing that no program names. Libraries a program opens only when they are there (gl,
    codesets) may stay unlisted; the C runtime may not."""
    required = {r["id"] for r in meta.get("requires_system", []) if r.get("type") == "library"}
    named = library_names(programs)
    unlisted = sorted((named & C_RUNTIME) - required)
    if unlisted:
        raise Failure(f"the programs open {', '.join(unlisted)} at start, but the catalogue description "
                      f"does not require {'it' if len(unlisted) == 1 else 'them'}")
    foreign = sorted(required - named)
    if foreign:
        raise Failure(f"the catalogue description requires {', '.join(foreign)}, which no program names: "
                      "a description copied from another target?")


def package_name(release, suffix):
    """<App>-<release>[-rN]-<suffix>: the packaging revision, when there is one, sits in every
    archive name so that a corrected package never has the name of the one it corrects."""
    revision = release.get("revision")
    if revision is not None and (not isinstance(revision, int) or revision < 1):
        raise Failure(f"release.json: revision {revision!r} is not a positive integer")
    return f"{release['name']}-{release['release']}{f'-r{revision}' if revision else ''}-{suffix}"


def apkg_pack_zip(path, files, date, target, release, programs):
    """Write the archive through arospkg's apkg-pack: it checks the catalogue description against
    the drawer, adds the description and the launch record, and checks the archive it wrote.
    Every other member must come out byte for byte as staged."""
    tool = os.environ.get("APKG_PACK")
    if not tool:
        raise Failure("set APKG_PACK to arospkg's tools/apkg-pack (or apkg-pack.pyz)")
    arch, abi = CATALOGUE[target]
    desc_path = PACKAGING / "catalogue" / f"{ID}.{arch}.{abi}.toml"
    desc = desc_path.read_bytes()
    meta = tomllib.loads(desc.decode())
    if (meta["version"], meta["arch"], meta["abi"]) != (release["release"], arch, abi):
        raise Failure(f"{desc_path.name}: version/arch/abi {meta['version']}/{meta['arch']}/{meta['abi']} "
                      f"is not {release['release']}/{arch}/{abi}")
    if meta.get("revision") != release.get("revision"):
        raise Failure(f"{desc_path.name}: revision {meta.get('revision')!r} is not release.json's {release.get('revision')!r}")
    if meta.get("source", {}).get("archive") != release["source_archive"]:
        raise Failure(f"{desc_path.name}: source.archive {meta.get('source', {}).get('archive')!r} is not {release['source_archive']!r}")
    try:
        check_requirements(meta, programs)
    except Failure as e:
        raise Failure(f"{desc_path.name}: {e}")
    drawer = release["drawer"]
    epoch = str(calendar.timegm((*date, 0, 0, 0)))
    partial = path.with_name(path.name + ".partial")
    with tempfile.TemporaryDirectory(prefix="craft-pack-") as tmp:
        stage = Path(tmp)
        for name, (data, mode) in files.items():
            (stage / name).parent.mkdir(parents=True, exist_ok=True)
            (stage / name).write_bytes(data)
            (stage / name).chmod(mode & 0o777)
        (stage / f"{drawer}.arospkg.toml").write_bytes(desc)
        for cmd in (["check", stage / drawer], ["build", stage / drawer, "--output", partial, "--epoch", epoch],
                    ["check", partial]):
            done = subprocess.run([tool, *map(str, cmd)], capture_output=True, text=True)
            if done.returncode:
                raise Failure(f"apkg-pack {cmd[0]} failed:\n{done.stdout}{done.stderr}")
    with zipfile.ZipFile(partial) as z:
        members = {n for n in z.namelist() if not n.endswith("/")}
        if members != set(files) | PACK_ADDED:
            raise Failure(f"apkg-pack archive members differ: {sorted(members ^ (set(files) | PACK_ADDED))}")
        changed = [n for n in files if z.read(n) != files[n][0]]
        if changed or z.read(".arospkg/manifest.toml") != desc:
            raise Failure(f"apkg-pack changed staged files: {changed or ['.arospkg/manifest.toml']}")
        added = {n: z.read(n) for n in PACK_ADDED}
    partial.rename(path)
    return added


class Dist:
    def __init__(self, path):
        self.path = path
        self.sums = path / "SHA256SUMS"

    def claim(self, name):
        listed = {line.split()[-1] for line in self.sums.read_text().splitlines() if line.strip()} if self.sums.exists() else set()
        if (self.path / name).exists() or (self.path / (name + ".partial")).exists() or name in listed:
            raise Failure(f"{self.path / name} exists or is listed in SHA256SUMS; a delivered file is never replaced")
        self.path.mkdir(parents=True, exist_ok=True)

    def record(self, name):
        digest = sha256((self.path / name).read_bytes())
        existing = self.sums.read_text().splitlines() if self.sums.exists() else []
        self.sums.write_text("\n".join([*existing, f"{digest}  {name}"]) + "\n")
        return digest


def cmd_binary(a, release):
    if len(a.build) != 1:
        raise Failure("binary takes one build output")
    build = a.build[0].resolve()
    manifest, spec = check_build(build, release)
    target = manifest["target"]
    cfg = config_for(target)
    repo_head = head_clean()
    dist = Dist(a.dist)
    name = package_name(release, f"{target}.zip")
    dist.claim(name)
    date = time.gmtime(int(git("log", "-1", "--format=%ct").decode().strip()))[:6]
    drawer = release["drawer"]
    files = {}
    packaged = {}
    for binary in BINARIES:
        undefined = run([tool(cfg, "nm"), "-u", build / binary]).decode().split()
        if undefined:
            raise Failure(f"{binary} has undefined symbols: {undefined[:8]}")
        packaged[binary] = stripped(cfg, (build / binary).read_bytes(), binary)
        files[f"{drawer}/{binary}"] = (packaged[binary], 0o100755)
    source = WORK / release["icon_source"].removeprefix("work:")
    w, h, rows = icons.decode_rgba(source)
    files[f"{drawer}.info"] = (icons.icon(w, h, rows, icons.ICON_DRAWER, release["stack"]), 0o100644)
    files[f"{drawer}/{drawer}.info"] = (icons.icon(w, h, rows, icons.ICON_TOOL, release["stack"], release["tooltypes"]), 0o100644)
    files[f"{drawer}/ReadMe.txt"] = (readme(release, spec), 0o100644)
    files[f"{drawer}/BUILD.txt"] = (build_txt(release, spec, manifest, repo_head, packaged).encode(), 0o100644)
    for rel in release["notices"]:
        files[f"{drawer}/Licenses/{APP}/{rel}"] = ((WORK / rel).read_bytes(), 0o100644)
    files[f"{drawer}/Licenses/AROS-NOTICE.txt"] = ((PACKAGING / spec["notice"]).read_bytes(), 0o100644)
    files[f"{drawer}/Licenses/AROS-LICENSE.txt"] = ((PACKAGING / "AROS-LICENSE.txt").read_bytes(), 0o100644)
    text, crates = third_party(manifest, Path(cfg["build_root"]))
    files[f"{drawer}/Licenses/THIRD-PARTY.txt"] = (text.encode(), 0o100644)
    for rel, data in rust_docs(cfg, manifest).items():
        files[f"{drawer}/Licenses/{rel}"] = (data, 0o100644)
    long_names = sorted({part for n in files for part in n.split("/") if len(part) > NAME_LIMIT})
    if long_names:
        raise Failure(f"archive names longer than {NAME_LIMIT} characters: {long_names}")
    for n, data in apkg_pack_zip(a.dist / name, files, date, target, release, packaged).items():
        files[n] = (data, 0o100644)
    digest = dist.record(name)
    record = {"archive": name, "sha256": digest, "bytes": (a.dist / name).stat().st_size, "target": target,
              "build_id": manifest["build_id"], "repository_head": repo_head, "work_head": manifest["work_head"],
              "crates": crates, "entries": {n: sha256(d) for n, (d, _) in sorted(files.items())},
              "packaged_binaries": {n: sha256(d) for n, d in packaged.items()}}
    (build / "package.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({k: record[k] for k in ("archive", "sha256", "bytes", "target", "build_id", "packaged_binaries")}, indent=2))


def git_tar(checkout, commit, paths=()):
    return lzma.compress(git("archive", "--format=tar", commit, *paths, cwd=checkout))


# Where each bundled input comes from, for INDEX.json. Commits are the ones the builds used.
ORIGINS = {
    "egui-aros": "https://github.com/tomaszstaniak/egui-aros",
    "rust-aros": "https://github.com/tomaszstaniak/rust-aros",
    "aros-v11": "https://github.com/deadwood2/AROS",
    "aros-mainline": "https://github.com/aros-development-team/AROS",
    "pomaros-hosted": "https://github.com/pomaros/pomaros-hosted",
}


def rust_licenses(rustc):
    """Rust's copyright notice and licence texts for the std source, from the rustup toolchain
    whose rustc reports exactly `rustc` (the std source itself carries none at its top)."""
    home = Path(os.environ.get("RUSTUP_HOME", Path.home() / ".rustup"))
    for toolchain in sorted((home / "toolchains").glob("*")):
        binary = toolchain / "bin" / "rustc"
        doc = toolchain / "share" / "doc" / "rust"
        if binary.exists() and (doc / "licenses").is_dir():
            version = subprocess.run([binary, "-V"], capture_output=True, text=True).stdout.strip()
            if version == rustc:
                files = {"COPYRIGHT-library.html": doc / "COPYRIGHT-library.html"}
                files.update({f"licenses/{f.name}": f for f in sorted((doc / "licenses").iterdir())})
                return files
    raise Failure(f"no rustup toolchain with licence texts reports {rustc!r}")


def cmd_source(a, release):
    builds = [b.resolve() for b in a.build]
    manifests = [check_build(b, release)[0] for b in builds]
    targets = [m["target"] for m in manifests]
    if len(set(targets)) != len(targets):
        raise Failure(f"one build per target, got {targets}")
    if len({m["work_head"] for m in manifests}) != 1:
        raise Failure("the builds come from different work copy commits")
    repo_head = head_clean()
    dist = Dist(a.dist)
    name = release["source_archive"]
    if name != package_name(release, "source.zip"):
        raise Failure(f"release.json: source_archive {name!r} should be {package_name(release, 'source.zip')!r}")
    dist.claim(name)
    prefix = name.removesuffix(".zip") + "/"
    path = a.dist / name
    partial = path.with_name(path.name + ".partial")
    excludes = [f":(exclude){x}" for x in release["source_archive_exclude"]]
    run(["git", "-C", REPO, "archive", "--format=zip", f"--prefix={prefix}", "-o", partial, "HEAD", "--", ".", *excludes])
    added = {}
    date = time.gmtime(int(git("log", "-1", "--format=%ct").decode().strip()))[:6]
    with zipfile.ZipFile(partial, "a") as bundle:
        def add(label, data):
            if label in added:
                if added[label] != sha256(data):
                    raise Failure(f"two different inputs named {label}")
                return
            added[label] = sha256(data)
            zip_entry(bundle, prefix + "sources/" + label, data, 0o100644, date)

        m0 = manifests[0]
        origins = {}
        add(f"{ID}-upstream.tar.xz", git_tar(REPO / "upstream" / ID, m0["upstreams"][ID]["commit"]))
        origins[f"{ID}-upstream.tar.xz"] = {"repository": m0["upstreams"][ID]["url"], "commit": m0["upstreams"][ID]["commit"],
                                                  "license": "MIT OR Apache-2.0", "content": f"the whole tree; the port's changes are patches/{ID}"}
        stds = {}
        for manifest in manifests:
            cfg = config_for(manifest["target"])
            # The tools every build ran (scripts/tooling.json's pin), so the archive builds without
            # the network: unpack as a checkout, or name the directory with CRAFT_TOOLING_DIR.
            used = manifest.get("tooling")
            if not used or not used.get("pinned"):
                raise Failure(f"build {manifest['build_id']} was not made with the pinned tools; rebuild with scripts/tooling.json's snapshot")
            add(f"craft-tools-{used['commit'][:12]}.tar.xz", git_tar(Path(cfg["egui_aros"]), used["commit"], [used["path"], "LICENSE-MIT", "LICENSE-APACHE"]))
            origins[f"craft-tools-{used['commit'][:12]}.tar.xz"] = {
                "repository": used["url"], "commit": used["commit"], "license": "MIT OR Apache-2.0",
                "content": f"{used['path']} (content hash {used['tree_sha256']}), LICENSE-MIT, LICENSE-APACHE"}
            add("egui-aros.tar.xz", git_tar(Path(cfg["egui_aros"]), manifest["upstreams"]["egui-aros"]["commit"]))
            origins["egui-aros.tar.xz"] = {"repository": ORIGINS["egui-aros"], "commit": manifest["upstreams"]["egui-aros"]["commit"],
                                           "license": "MIT OR Apache-2.0", "content": "the whole tree"}
            libc = manifest["libc_aros"]
            commit = libc.get("rust_aros_commit", cfg["rust_aros_commit"])
            add(f"rust-aros-{commit[:12]}.tar.xz",
                git_tar(Path(cfg["rust_aros"]), commit, ["LICENSE", "std-groundwork/libc-aros", "std-groundwork/std-patch", "x86_64-aros.json.in", "profiles"]))
            origins[f"rust-aros-{commit[:12]}.tar.xz"] = {
                "repository": ORIGINS["rust-aros"], "commit": commit, "license": "MIT",
                "content": "LICENSE, std-groundwork/libc-aros, std-groundwork/std-patch, x86_64-aros.json.in, profiles"}
            if manifest.get("pthread") and "aros_src_commit" in manifest["pthread"]:
                pc = manifest["pthread"]["aros_src_commit"]
                add(f"aros-pthread-{pc[:12]}.tar.xz", git_tar(Path(cfg["aros_src"]), pc, ["compiler/pthread", "LICENSE"]))
                origins[f"aros-pthread-{pc[:12]}.tar.xz"] = {
                    "repository": ORIGINS["aros-v11" if manifest["target"].endswith("abiv11-pc") else "aros-mainline"],
                    "commit": pc, "license": "AROS Public License (LICENSE)", "content": "compiler/pthread, LICENSE",
                    "patches": sorted(manifest["pthread"].get("patches", {}))}
            if manifest.get("glue"):
                gc = manifest["glue"]["pomaros_hosted_commit"]
                add(f"pomaros-hosted-{gc[:12]}.tar.xz", git_tar(Path(cfg["pomaros_hosted"]), gc, ["hosted/rust", "LICENSE"]))
                origins[f"pomaros-hosted-{gc[:12]}.tar.xz"] = {
                    "repository": ORIGINS["pomaros-hosted"], "commit": gc, "license": "see LICENSE", "content": "hosted/rust, LICENSE",
                    "patches": sorted(manifest["glue"].get("patches", {}))}
            key = manifest["toolchain"]["std_source_sha256"]
            if key not in stds:
                packed = io.BytesIO()
                with tarfile.open(fileobj=packed, mode="w:xz") as tar:
                    tar.add(Path(manifest["toolchain"]["sysroot"]) / "lib/rustlib/src/rust", arcname="rust", recursive=True)
                    for licence, source in rust_licenses(manifest["toolchain"]["rustc"]).items():
                        tar.add(source, arcname=f"rust/{licence}")
                stds[key] = f"rust-std-{key[:12]}.tar.xz"
                origins[stds[key]] = {
                    "repository": "https://github.com/rust-lang/rust", "toolchain": manifest["toolchain"]["rustc"],
                    "license": "MIT OR Apache-2.0 (rust/COPYRIGHT-library.html, rust/licenses)",
                    "content": "the std source of the build sysroot: rust-src of that toolchain with rust-aros std-groundwork/std-patch applied"
                               + ("; library/libc-aros is the AArch64 adaptation of rust-aros libc-aros that this target links"
                                  if "path" in manifest["libc_aros"] else "")}
                add(stds[key], packed.getvalue())
            for info in manifest["crates"].values():
                crate = f"{info['package']}-{info['version']}.crate"
                found = list(Path(cfg["cargo_home"]).glob("registry/cache/*/" + crate))
                if len(found) != 1:
                    raise Failure(f"{crate} is not in {cfg['cargo_home']}")
                add("crates/" + crate, found[0].read_bytes())
                origins["crates/" + crate] = {"repository": "https://crates.io", "content": "the registry archive; patched at build time"}
        index = {"repository_head": repo_head, "work_head": m0["work_head"],
                 "builds": {m["target"]: m["build_id"] for m in manifests},
                 "rust_std": stds, "files": dict(sorted(added.items())), "origins": dict(sorted(origins.items()))}
        zip_entry(bundle, prefix + "sources/INDEX.json", (json.dumps(index, indent=2) + "\n").encode(), 0o100644, date)
    partial.rename(path)
    digest = dist.record(name)
    print(json.dumps({"archive": str(path), "sha256": digest, "targets": targets, "inputs": sorted(added)}, indent=2))


def main(repo, argv, tooling=None):
    """Packages for the port in `repo`; returns the process status."""
    try:
        configure(repo, tooling)
    except craftconfig.ConfigError as e:
        print(f"package: {e}", file=sys.stderr)
        return 1
    p = argparse.ArgumentParser(prog="craft.py package", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("kind", choices=["binary", "source"])
    p.add_argument("build", type=Path, nargs="+")
    p.add_argument("--dist", type=Path, default=REPO / "dist")
    a = p.parse_args(argv)
    a.dist = a.dist.resolve()
    release = json.loads((PACKAGING / "release.json").read_text())
    try:
        (cmd_binary if a.kind == "binary" else cmd_source)(a, release)
    except (Failure, craftconfig.ConfigError, subprocess.CalledProcessError) as e:
        print(f"package: {e}", file=sys.stderr)
        return 1
    return 0
