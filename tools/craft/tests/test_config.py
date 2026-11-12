"""The shared tools serve any port they are told about, and a port's bootstrap runs exactly the
tools its tooling.json pins."""
import contextlib
import io
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import support  # noqa: E402
import bootstrap  # noqa: E402
import craft  # noqa: E402
import craftconfig  # noqa: E402
import package  # noqa: E402

WORKSTATION_KEYS = ("aros_gcc", "aros_ar", "aros_strip", "aros_sdk", "rust_aros", "aros_src", "pomaros_hosted", "egui_aros",
                    "egui_upstream_root", "toolchain")


class Temp(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="craft tools ")).resolve()
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        saved = dict(os.environ)
        self.addCleanup(lambda: (os.environ.clear(), os.environ.update(saved)))
        for key in list(os.environ):
            if key.startswith(("GRIDCRAFT_", "WORDCRAFT_", "CRAFT_")):
                del os.environ[key]


class Config(Temp):
    def test_one_implementation_serves_two_ports(self):
        grid = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT")
        word = support.port(self.root, "wordcraft", "WordCraft", "WORDCRAFT")
        (grid / "local.json").write_text(json.dumps({"toolchain": "grid-toolchain"}))
        (word / "local.json").write_text(json.dumps({"toolchain": "word-toolchain"}))
        os.environ["WORDCRAFT_TARGET"] = "aros-x86_64-abiv1-pc"
        craftconfig.configure(grid)
        cfg = craftconfig.load(["toolchain"])
        self.assertEqual(cfg["toolchain"], "grid-toolchain")
        self.assertEqual(cfg["target"], "aros-x86_64-abiv11-pc", "another port's variable must not reach this one")
        self.assertEqual(Path(cfg["build_root"]), (grid / "build" / "aros-x86_64-abiv11-pc").resolve())
        craftconfig.configure(word)
        cfg = craftconfig.load(["toolchain"])
        self.assertEqual(cfg["toolchain"], "word-toolchain")
        self.assertEqual(cfg["target"], "aros-x86_64-abiv1-pc")
        self.assertEqual(Path(cfg["build_root"]), (word / "build" / "aros-x86_64-abiv1-pc").resolve())

    def test_relative_paths_belong_to_the_file_that_names_them(self):
        grid = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT")
        (grid / "local.json").write_text(json.dumps({"aros_sdk": "sdk here"}))
        craftconfig.configure(grid)
        self.assertEqual(Path(craftconfig.load()["aros_sdk"]), (grid / "sdk here").resolve())
        elsewhere = self.root / "profiles"
        elsewhere.mkdir()
        (elsewhere / "v1.json").write_text(json.dumps({"aros_sdk": "sdk v1"}))
        os.environ["GRIDCRAFT_LOCAL"] = str(elsewhere / "v1.json")
        self.assertEqual(Path(craftconfig.load()["aros_sdk"]), (elsewhere / "sdk v1").resolve())
        os.environ["GRIDCRAFT_AROS_SDK"] = "from env"
        self.assertEqual(Path(craftconfig.load()["aros_sdk"]), (grid / "from env").resolve())

    def test_workstation_paths_have_no_defaults(self):
        grid = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT")
        craftconfig.configure(grid)
        cfg = craftconfig.load()
        for key in WORKSTATION_KEYS:
            self.assertIsNone(cfg[key], key)
        with self.assertRaises(craftconfig.ConfigError) as caught:
            craftconfig.load(["aros_gcc"])
        self.assertIn("GRIDCRAFT_AROS_GCC", str(caught.exception))

    def test_the_tools_do_not_take_the_repository_from_their_own_location(self):
        grid = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(craft.main(grid, ["config"]), 0)
        self.assertIn("GRIDCRAFT_TARGET", out.getvalue())
        self.assertEqual(craft.REPO, grid)
        self.assertEqual(craft.SERIES_DIR, grid / "patches" / "gridcraft")
        self.assertNotIn(str(support.TOOLS), str(craft.WORK))

    def test_a_failing_command_is_a_status_not_an_exit(self):
        grid = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT")
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(craft.main(grid, ["verify"]), 1)
        self.assertIn("craft.py verify:", err.getvalue())


class Package(Temp):
    def test_packaging_finds_each_ports_local_file_by_its_target(self):
        grid = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT")
        word = support.port(self.root, "wordcraft", "WordCraft", "WORDCRAFT")
        needed = {k: "x" for k in ("aros_gcc", "aros_strip", "aros_sdk", "rust_aros", "toolchain")}
        (grid / "local.json").write_text(json.dumps(dict(needed, toolchain="grid-v11")))
        (grid / "local.v1.json").write_text(json.dumps(dict(needed, toolchain="grid-v1", target="aros-x86_64-abiv1-pc")))
        (grid / "local.example.json").write_text(json.dumps({"target": "aros-x86_64-abiv1-pc"}))
        (word / "local.v1.json").write_text(json.dumps(dict(needed, toolchain="word-v1", target="aros-x86_64-abiv1-pc")))
        package.configure(grid)
        self.assertEqual(package.APP, "GridCraft")
        self.assertEqual(package.BINARIES, ("GridCraft", "gridcraft-cli"))
        self.assertEqual(package.config_for("aros-x86_64-abiv1-pc")["toolchain"], "grid-v1")
        self.assertEqual(package.config_for("aros-x86_64-abiv11-pc")["toolchain"], "grid-v11")
        self.assertNotIn("GRIDCRAFT_LOCAL", os.environ)
        package.configure(word)
        self.assertEqual(package.config_for("aros-x86_64-abiv1-pc")["toolchain"], "word-v1")
        with self.assertRaises(package.Failure):
            package.config_for("aros-aarch64-abiv1-raspi")

    def test_packaging_is_a_command_of_the_same_tools(self):
        grid = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT")
        out = io.StringIO()
        with contextlib.redirect_stdout(out), self.assertRaises(SystemExit) as caught:
            craft.main(grid, ["package", "--help"])
        self.assertEqual(caught.exception.code, 0)
        self.assertIn("craft.py package", out.getvalue())


class Bootstrap(Temp):
    """scripts/craft.py of a port (tools/craft/bootstrap.py, copied) against a throwaway tools
    repository whose craft.py prints which snapshot ran."""

    STUB = "def main(repo, argv, tooling=None):\n    print('{mark}', repo.name, ' '.join(argv), tooling['commit'], tooling['pinned'])\n    return 0\n"

    def setUp(self):
        super().setUp()
        self.source = support.init(self.root / "tools source")
        self.first = support.commit(self.source, {"tools/craft/craft.py": self.STUB.format(mark="first"), "README.md": "x"}, "first")
        self.second = support.commit(self.source, {"tools/craft/craft.py": self.STUB.format(mark="second")}, "second")
        self.port = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT")
        shutil.copy2(support.TOOLS / "bootstrap.py", self.port / "scripts" / "craft.py")
        self.env = dict(support.clean_env("GRIDCRAFT", "WORDCRAFT"), **support.GIT_ENV)

    def pin(self, commit, digest=None, url="file:///nonexistent/tools.git"):
        if digest is None:
            digest = bootstrap.tree_hash_of_commit(self.source, commit, "tools/craft")
        (self.port / "scripts" / "tooling.json").write_text(json.dumps({"url": url, "commit": commit, "path": "tools/craft", "tree_sha256": digest,
                                                                         "local_key": "egui_aros"}))

    def run_port(self, *args, **env):
        return support.run_tool(self.port / "scripts" / "craft.py", *args, env=dict(self.env, **env), cwd=self.root)

    def test_the_pinned_snapshot_runs_with_the_port_as_its_repository(self):
        self.pin(self.first)
        result = self.run_port("config", "--x", CRAFT_TOOLING_SOURCE=str(self.source))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f"first gridcraft config --x {self.first} True")

    def test_a_new_pin_is_a_new_snapshot(self):
        self.pin(self.first)
        self.run_port("config", CRAFT_TOOLING_SOURCE=str(self.source))
        self.pin(self.second)
        result = self.run_port("config", CRAFT_TOOLING_SOURCE=str(self.source))
        self.assertEqual(result.stdout.split()[0], "second", result.stderr)
        self.pin(self.first)
        result = self.run_port("config", CRAFT_TOOLING_SOURCE=str(self.source))
        self.assertEqual(result.stdout.split()[0], "first", "an older release still gets its own tools")

    def test_a_snapshot_with_the_wrong_content_is_refused(self):
        self.pin(self.first, digest="0" * 64)
        result = self.run_port("config", CRAFT_TOOLING_SOURCE=str(self.source))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tree_sha256", result.stderr)
        self.assertNotIn("first", result.stdout)
        cache = self.port / "build" / "tooling"
        self.assertFalse(cache.exists() and any(p.name == "craft.py" for p in cache.rglob("*")), "nothing unverified stays in the cache")

    def test_a_cached_snapshot_that_was_edited_is_refused(self):
        self.pin(self.first)
        self.run_port("config", CRAFT_TOOLING_SOURCE=str(self.source))
        cached = next((self.port / "build" / "tooling").rglob("craft.py"))
        cached.write_text(self.STUB.format(mark="edited"))
        result = self.run_port("config", CRAFT_TOOLING_SOURCE=str(self.source))
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("edited", result.stdout)

    def test_offline_the_local_checkout_named_in_the_local_file_is_enough(self):
        self.pin(self.first)
        (self.port / "local.json").write_text(json.dumps({"egui_aros": str(self.source)}))
        result = self.run_port("config")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.split()[0], "first")

    def test_the_local_file_named_by_the_environment_is_read(self):
        self.pin(self.first)
        profile = self.root / "v1.json"
        profile.write_text(json.dumps({"egui_aros": str(self.source)}))
        result = self.run_port("config", GRIDCRAFT_LOCAL=str(profile))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_url_is_used_when_nothing_local_is_configured(self):
        self.pin(self.first, url=self.source.as_uri())
        result = self.run_port("config")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.split()[0], "first")

    def test_no_source_is_reported_with_the_way_to_set_one(self):
        self.pin(self.first)
        result = self.run_port("config")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CRAFT_TOOLING_SOURCE", result.stderr)

    def test_a_moving_reference_is_not_a_pin(self):
        self.pin("main", digest="0" * 64)
        result = self.run_port("config", CRAFT_TOOLING_SOURCE=str(self.source))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("40", result.stderr)

    def test_a_tools_directory_under_development_is_named_as_unpinned(self):
        self.pin(self.first)
        live = self.root / "live tools"
        live.mkdir()
        (live / "craft.py").write_text(self.STUB.format(mark="live"))
        result = self.run_port("config", CRAFT_TOOLING_DIR=str(live))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.split()[0], "live")
        self.assertEqual(result.stdout.split()[-1], "False")
        self.assertIn("unpinned", result.stderr)


if __name__ == "__main__":
    unittest.main()
