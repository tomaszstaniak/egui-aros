"""Throwaway Git repositories and port layouts for the tool tests."""
import json
import os
import subprocess
import sys
from pathlib import Path

TOOLS = Path(__file__).resolve().parents[1]
GIT_ENV = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@invalid", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@invalid",
           "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
           "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_SYSTEM": os.devnull}


def git(repo, *args):
    result = subprocess.run(["git", "-C", str(repo), *args], env=dict(os.environ, **GIT_ENV), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        raise AssertionError(f"git {' '.join(args)}: {result.stderr.decode()}")
    return result.stdout.decode().strip()


def init(path):
    path.mkdir(parents=True)
    git(path, "init", "--quiet", "--initial-branch=main")
    return path


def commit(repo, files, message):
    """Writes `files` ({relative name: text, or None to delete}), commits, returns the commit."""
    for name, text in files.items():
        target = repo / name
        if text is None:
            target.unlink()
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)
    git(repo, "add", "--all")
    git(repo, "commit", "--quiet", "--allow-empty", "-m", message)
    return git(repo, "rev-parse", "HEAD")


def port(root, ident, name, prefix, upstream_url="https://example.invalid/app.git", commit_id="0" * 40):
    """A port repository layout without sources: scripts/port.json, upstreams.json, patches/<id>."""
    repo = root / ident
    (repo / "scripts").mkdir(parents=True)
    (repo / "scripts" / "port.json").write_text(json.dumps({
        "id": ident, "name": name, "env_prefix": prefix,
        "packages": {ident + "-aros": name, ident + "-cli": ident + "-cli"}, "tools": {}}))
    (repo / "upstreams.json").write_text(json.dumps({"repositories": {ident: {"url": upstream_url, "commit": commit_id}}}, indent=2) + "\n")
    (repo / "patches" / ident).mkdir(parents=True)
    (repo / "patches" / ident / "series").write_text("")
    return repo


def clean_env(*prefixes):
    """The environment without any port's settings, so the host's own do not leak into a test."""
    return {k: v for k, v in os.environ.items() if not k.startswith(tuple(p + "_" for p in prefixes)) and not k.startswith("CRAFT_")}


def run_tool(script, *args, env=None, cwd=None):
    return subprocess.run([sys.executable, str(script), *args], env=env, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
