"""update-check: what moving a port to another upstream commit would take.

The series is tried on the named commit in a directory of its own, and the dependency changes
between the pinned and the named commit are listed. The result is a report: the port's pins,
patches, work copy and upstream checkout are read, never written, and nothing is promoted. A
patch is never dropped here either; `upstream-equivalent` says that its change is already in the
candidate's files, which is a reason to check the behaviour, not proof that it can go.

Patch status:
  applies              applies as it is
  applies-3way         applies with a three-way merge (context moved)
  upstream-equivalent  reverses cleanly on the candidate: its change is already there; left out
  conflicts            does not apply; left out, with the files it touches or conflicts in

A patch left out changes what the later ones meet, so statuses after the first `conflicts` hold
for the series without it.
"""
import json
import re
import shutil
import tempfile
import tomllib
from pathlib import Path

FULL_COMMIT = re.compile(r"[0-9a-f]{40}")
GIT_PATCH = re.compile(r'^([A-Za-z0-9_-]+) = \{ git = "[^"]+", rev = "([0-9a-f]+)"', re.M)


def _locked(text):
    """{package: sorted versions} of a Cargo.lock."""
    out = {}
    for package in tomllib.loads(text).get("package", []):
        out.setdefault(package["name"], set()).add(package["version"])
    return {name: sorted(versions) for name, versions in out.items()}


def _show(craft, repo, commit, name):
    result = craft.run(["git", "-C", repo, "show", f"{commit}:{name}"], check=False)
    return result.stdout.decode(errors="replace") if result.returncode == 0 else ""


def _patch_files(path):
    names = []
    for line in Path(path).read_text(errors="replace").splitlines():
        if line.startswith("diff --git a/"):
            name = line.split(" b/", 1)[1]
            if name not in names:
                names.append(name)
    return names


def _dependencies(craft, repo, old, new):
    before, after = _locked(_show(craft, repo, old, "Cargo.lock")), _locked(_show(craft, repo, new, "Cargo.lock"))
    lock = {"added": {n: v for n, v in after.items() if n not in before},
            "removed": {n: v for n, v in before.items() if n not in after},
            "changed": {n: {"from": before[n], "to": after[n]} for n in before if n in after and before[n] != after[n]}}
    changed = craft.git(repo, "diff", "--name-only", old, new, "--", "Cargo.toml", "**/Cargo.toml")
    revs_before = dict(GIT_PATCH.findall(_show(craft, repo, old, "Cargo.toml")))
    revs_after = dict(GIT_PATCH.findall(_show(craft, repo, new, "Cargo.toml")))
    git_patches = {"added": {n: r for n, r in revs_after.items() if n not in revs_before},
                   "removed": {n: r for n, r in revs_before.items() if n not in revs_after},
                   "changed": {n: {"from": revs_before[n], "to": revs_after[n]} for n in revs_before
                               if n in revs_after and revs_before[n] != revs_after[n]}}
    crates = []
    crates_json = craft.CRATES_DIR / "crates.json"
    if crates_json.is_file():
        for spec in json.loads(crates_json.read_text())["crates"]:
            versions = after.get(spec["name"], [])
            crates.append({"name": spec["name"], "version": spec["version"], "locked": spec["version"] in versions,
                           "candidate_versions": versions})
    return lock, {"changed": [n for n in changed.splitlines() if n]}, git_patches, crates


def _try_series(craft, trial):
    """Applies what applies, in order, as commits of `trial`; returns each patch's status."""
    rows = []
    for name in craft.series():
        patch = craft.SERIES_DIR / name
        row = {"name": name, "files": _patch_files(patch)}
        if craft.run(["git", "-C", trial, "apply", "--check", "--reverse", patch], check=False).returncode == 0:
            row["status"] = "upstream-equivalent"
            rows.append(row)
            continue
        am = ["git", *craft.COMMITTER, "-C", trial, "am", "--quiet", "--keep-cr", "--whitespace=nowarn", "--committer-date-is-author-date"]
        if craft.run([*am, patch], check=False).returncode == 0:
            row["status"] = "applies"
            rows.append(row)
            continue
        craft.run(["git", "-C", trial, "am", "--abort"], check=False)
        if craft.run([*am, "--3way", patch], check=False).returncode == 0:
            row["status"] = "applies-3way"
            rows.append(row)
            continue
        unmerged = craft.git(trial, "diff", "--name-only", "--diff-filter=U")
        if unmerged:
            row["files"] = [n for n in unmerged.splitlines() if n]
        craft.run(["git", "-C", trial, "am", "--abort"], check=False)
        row["status"] = "conflicts"
        rows.append(row)
    return rows


def check(craft, commit, output, keep=None):
    if not FULL_COMMIT.fullmatch(commit):
        raise craft.Failure("--upstream-commit must be a full 40-character commit, not a name that can move")
    if keep is not None and (keep.exists() or keep.is_symlink()):
        raise craft.Failure(f"{keep} exists; --keep names a new directory and never takes one over")
    spec = craft.upstreams()[craft.APP]
    # Only the checkout's objects are read (it is cloned without its files), so it may stand at
    # another commit than the pin, as it does once a candidate has been looked at.
    base = craft.REPO / "upstream" / craft.APP
    if craft.run(["git", "-C", base, "cat-file", "-e", spec["commit"] + "^{commit}"], check=False).returncode:
        raise craft.Failure(f"upstream/{craft.APP} does not hold the pinned commit {spec['commit']}; run `fetch` first")
    with tempfile.TemporaryDirectory(prefix=f"{craft.APP} update ") as tmp:
        trial = Path(tmp) / "trial"
        # A clone of the port's checkout: the candidate commit, when the checkout does not have it,
        # is fetched into the clone, so the port's own upstream/<id> gains no objects.
        craft.run(["git", "clone", "--quiet", "--no-checkout", base, trial])
        if craft.run(["git", "-C", trial, "cat-file", "-e", commit + "^{commit}"], check=False).returncode:
            craft.run(["git", "-C", trial, "fetch", "--quiet", spec["url"], "+refs/heads/*:refs/remotes/candidate/*"], check=False)
            if craft.run(["git", "-C", trial, "cat-file", "-e", commit + "^{commit}"], check=False).returncode:
                craft.run(["git", "-C", trial, "fetch", "--quiet", spec["url"], commit], check=False)
        if craft.run(["git", "-C", trial, "cat-file", "-e", commit + "^{commit}"], check=False).returncode:
            raise craft.Failure(f"{spec['url']} has no commit {commit}")
        craft.run(["git", "-C", trial, "-c", "advice.detachedHead=false", "checkout", "--quiet", "-b", "update-check", commit])
        between = int(craft.git(trial, "rev-list", "--count", f"{spec['commit']}..{commit}"))
        behind = int(craft.git(trial, "rev-list", "--count", f"{commit}..{spec['commit']}"))
        lock, manifests, git_patches, crates = _dependencies(craft, trial, spec["commit"], commit)
        patches = _try_series(craft, trial)
        work = {"exists": craft.WORK.exists()}
        if work["exists"]:
            head, status = craft.work_state(craft.WORK)
            work.update(head=head, dirty=bool(status))
        counts = {}
        for row in patches:
            counts[row["status"]] = counts.get(row["status"], 0) + 1
        report = {
            "app": craft.APP,
            "current": {"commit": spec["commit"]},
            "candidate": {"commit": commit, "commits_ahead": between, "commits_not_in_candidate": behind,
                          "version": _workspace_version(_show(craft, trial, commit, "Cargo.toml"))},
            "patches": patches,
            "summary": counts,
            "lock": lock,
            "manifests": manifests,
            "git_patches": git_patches,
            "patched_crates": crates,
            "work": work,
            "promoted": False,
            "tooling": craft.TOOLING,
        }
        if keep is not None:
            keep.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(trial), str(keep))
            report["trial"] = str(keep)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    return report


def _workspace_version(manifest):
    try:
        data = tomllib.loads(manifest)
    except tomllib.TOMLDecodeError:
        return None
    return data.get("workspace", {}).get("package", {}).get("version") or data.get("package", {}).get("version")


def summary(report):
    lines = [f"{report['app']}: {report['current']['commit'][:12]} -> {report['candidate']['commit'][:12]} "
             f"({report['candidate']['commits_ahead']} commits, version {report['candidate']['version'] or 'unknown'})"]
    for row in report["patches"]:
        files = ", ".join(row["files"][:4]) + (" ..." if len(row["files"]) > 4 else "")
        lines.append(f"  {row['status']:20} {row['name']}" + (f"  [{files}]" if row["status"] == "conflicts" else ""))
    lock = report["lock"]
    lines.append(f"  Cargo.lock: {len(lock['added'])} added, {len(lock['removed'])} removed, {len(lock['changed'])} changed; "
                 f"{len(report['manifests']['changed'])} manifests changed")
    for name, change in report["git_patches"]["changed"].items():
        lines.append(f"  [patch] {name}: {change['from'][:12]} -> {change['to'][:12]}")
    stale = [f"{c['name']} {c['version']} (candidate: {', '.join(c['candidate_versions']) or 'not locked'})"
             for c in report["patched_crates"] if not c["locked"]]
    if stale:
        lines.append("  patched crates whose version the candidate does not lock: " + "; ".join(stale))
    lines.append("  nothing was changed in the port; pins are promoted by hand after review")
    return "\n".join(lines)
