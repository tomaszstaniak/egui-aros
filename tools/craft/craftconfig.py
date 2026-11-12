"""One place for a Craft port's paths and pins. Shared by the Craft ports: the port is named by
configure(), and its app by its scripts/port.json.

Values come from, in order of precedence: environment variables, the ignored
`local.json` beside `upstreams.json` (or the file named by <PREFIX>_LOCAL),
and the defaults below. Workstation paths have no defaults: a missing value is
reported with the variable that sets it, never guessed.
"""
import json
import os
from pathlib import Path


class ConfigError(Exception):
    pass


# Set by configure(): the port's repository, its scripts/port.json, its environment prefix and the
# settings table built with that prefix. The tools never take them from their own location: they
# run from a pinned snapshot outside the port.
REPO = None
PORT = None
P = None
SETTINGS = {}

# Build targets (<system>-<arch>-<abi>-<platform>) and their rust-aros profile. `aros_src_commit`
# is the AROS source whose compiler/pthread matches the target SDK's libpthread.a; `elf` is the
# (EI_ABIVERSION, e_machine) pair collect-aros writes for the target.
TARGETS = {
    "aros-x86_64-abiv11-pc": {"profile": "v11", "panic": "unwind",
                              "aros_src_commit": "5376f09bc1ed0c36a12fbb096406013f8a046e89"},
    "aros-x86_64-abiv1-pc": {"profile": "x86_64-v1-pc", "panic": "abort"},
    # Native Raspberry Pi SDK 20260822. Its pthread is mainline cc39c62 (rust-aros
    # docs/integration/runtime-arm-20261023.md); the newer Macaros source needs an errno hook
    # this SDK does not export.
    "aros-aarch64-abiv1-raspi": {"profile": "aarch64-v1-raspi", "panic": "abort",
                                 "aros_src_commit": "cc39c62f33d90b842540e79d5e47357590c014ac",
                                 "elf": (1, 183)},
}


def configure(repo):
    """Names the port these tools work for: `repo` holds scripts/port.json and upstreams.json."""
    global REPO, PORT, P, SETTINGS
    REPO = Path(repo).resolve()
    try:
        PORT = json.loads((REPO / "scripts" / "port.json").read_text())
    except (OSError, ValueError) as error:
        raise ConfigError(f"{REPO / 'scripts' / 'port.json'}: {error}") from error
    P = PORT["env_prefix"]
    # key: (environment variable, default, description)
    SETTINGS = {
        "target": (P + "_TARGET", "aros-x86_64-abiv11-pc", "build target: " + ", ".join(sorted(TARGETS))),
        "aros_gcc": (P + "_AROS_GCC", None, "AROS C compiler driver matching the SDK (x86_64 GCC, aarch64 Clang)"),
        "aros_ar": (P + "_AROS_AR", None, "archiver from the same toolchain"),
        "aros_strip": (P + "_AROS_STRIP", None, "strip from the same toolchain (optional)"),
        "aros_sdk": (P + "_AROS_SDK", None, "native ABI-matched C SDK used as --sysroot"),
        "rust_aros": (P + "_RUST_AROS", None, "rust-aros Git checkout (target template, libc-aros)"),
        "rust_aros_commit": (P + "_RUST_AROS_COMMIT", "da569a68146b404de3e832f69217fa51ad719a22",
                             "rust-aros commit whose target template and libc-aros are used"),
        "aros_src": (P + "_AROS_SRC", None, "AROS Git checkout holding aros_src_commit; read-only source of the pthread link library "
                     "(v11: deadwood2/AROS; aarch64: mainline)"),
        "aros_src_commit": (P + "_AROS_SRC_COMMIT", None,
                            "commit whose compiler/pthread matches the SDK's libpthread.a (default: the target's)"),
        "pthread": (P + "_PTHREAD", "patched",
                    "pthread link library on v11 and aarch64: patched (patches/aros-pthread over the SDK's) or sdk (the "
                    "SDK's libpthread.a as is, when it already has mainline's fix c31d3294, e.g. nightlies from 2026-10-08) or "
                    "rebuilt (aarch64: the replaced members compiled here from aros_src_commit, unpatched)"),
        "pomaros_hosted": (P + "_POMAROS_HOSTED", None, "pomaros-hosted Git checkout: C bridge of the aarch64 std PAL (aarch64 only)"),
        "pomaros_hosted_commit": (P + "_POMAROS_HOSTED_COMMIT", "3bc9359d439f9fb5655cf48924413ffe670931cc",
                                  "pomaros-hosted commit whose hosted/rust bridge the common sysroot's aarch64 PAL was made from"),
        "egui_aros": (P + "_EGUI_AROS", None, "egui-aros Git checkout; its commit pinned in upstreams.json is built"),
        "egui_upstream_root": (P + "_EGUI_UPSTREAM_ROOT", None, "read-only prepared egui-aros upstream repositories"),
        "toolchain": (P + "_TOOLCHAIN", None, "rustup name of the private AROS std toolchain"),
        "rustc_version": (P + "_RUSTC_VERSION", "rustc 1.100.0-nightly (0dfb098f3 2026-08-31)",
                          "exact `rustc --version` the toolchain must report"),
        "cargo_home": (P + "_CARGO_HOME", None, "private CARGO_HOME (default deps/<target>/cargo-home; relative: to the repository)"),
        "build_root": (P + "_BUILD_ROOT", None, "build outputs (default build/<target>; relative: to the repository)"),
        "panic": (P + "_PANIC", None, "panic strategy (default: the target's)"),
    }


def _local():
    path = Path(os.environ.get(P + "_LOCAL", REPO / "local.json"))
    if not path.exists():
        return {}, None
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError) as error:
        raise ConfigError(f"{path}: {error}") from error
    unknown = set(data) - set(SETTINGS)
    if unknown:
        raise ConfigError(f"{path}: unknown keys {sorted(unknown)}")
    return data, path


def load(required=()):
    """Returns the resolved settings; raises ConfigError naming what is missing."""
    local, local_path = _local()
    out = {}
    for key, (env, default, _) in SETTINGS.items():
        value = os.environ.get(env) or local.get(key) or default
        out[key] = value
    if out["target"] not in TARGETS:
        raise ConfigError(f"target must be one of {sorted(TARGETS)}")
    out["profile"] = TARGETS[out["target"]]["profile"]
    out["cargo_home"] = out["cargo_home"] or f"deps/{out['target']}/cargo-home"
    out["build_root"] = out["build_root"] or f"build/{out['target']}"
    out["panic"] = out["panic"] or TARGETS[out["target"]]["panic"]
    out["aros_src_commit"] = out["aros_src_commit"] or TARGETS[out["target"]].get("aros_src_commit")
    out["elf"] = TARGETS[out["target"]].get("elf")
    paths = ("aros_gcc", "aros_ar", "aros_strip", "aros_sdk", "rust_aros", "aros_src", "pomaros_hosted", "egui_aros", "egui_upstream_root",
             "cargo_home", "build_root")
    for key in paths:
        if out[key] and not os.path.isabs(out[key]):
            base = REPO if os.environ.get(SETTINGS[key][0]) or key not in local else (local_path.parent if local_path else REPO)
            out[key] = str((base / out[key]).resolve())
    missing = [key for key in required if not out.get(key)]
    if missing:
        lines = [f"  {k}: set {SETTINGS[k][0]} or \"{k}\" in local.json ({SETTINGS[k][2]})" for k in missing]
        raise ConfigError("missing configuration:\n" + "\n".join(lines))
    if out["panic"] not in ("unwind", "abort"):
        raise ConfigError("panic must be unwind or abort")
    return out


def describe():
    local, path = _local()
    rows = []
    for key, (env, default, text) in SETTINGS.items():
        source = "env" if os.environ.get(env) else ("local" if key in local else ("default" if default else "unset"))
        rows.append(f"{key:18} {source:8} {env}  ({text})")
    return f"local file: {path or 'none'}\n" + "\n".join(rows)
