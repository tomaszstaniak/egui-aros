"""update-check tries the series on another upstream commit in a directory of its own and
reports; the port's pins, patches, work copy and upstream checkout stay as they were."""
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
import craft  # noqa: E402

LOCK_OLD = '[[package]]\nname = "foo"\nversion = "1.0.0"\n\n[[package]]\nname = "libc"\nversion = "0.2.1"\n'
LOCK_NEW = '[[package]]\nname = "bar"\nversion = "3.0.0"\n\n[[package]]\nname = "foo"\nversion = "1.1.0"\n\n[[package]]\nname = "libc"\nversion = "0.2.1"\n'
MANIFEST = '[workspace]\nmembers = ["crates/*"]\n\n[patch.crates-io]\negui = {{ git = "https://example.invalid/egui.git", rev = "{rev}" }}\n'


class UpdateCheck(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="craft update ")).resolve()
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        saved = dict(os.environ)
        self.addCleanup(lambda: (os.environ.clear(), os.environ.update(saved)))
        for key in list(os.environ):
            if key.startswith(("GRIDCRAFT_", "CRAFT_")):
                del os.environ[key]
        os.environ.update(support.GIT_ENV)
        self.upstream = support.init(self.root / "upstream source")
        self.old = support.commit(self.upstream, {"a.txt": "alpha\n", "b.txt": "one\ntwo\nthree\n", "Cargo.lock": LOCK_OLD,
                                                  "Cargo.toml": MANIFEST.format(rev="a" * 40)}, "old")
        self.port = support.port(self.root, "gridcraft", "GridCraft", "GRIDCRAFT", self.upstream.as_uri(), self.old)
        checkout = self.port / "upstream" / "gridcraft"
        checkout.parent.mkdir()
        support.git(self.root, "clone", "--quiet", self.upstream.as_uri(), str(checkout))
        support.git(checkout, "-c", "advice.detachedHead=false", "checkout", "--quiet", "--detach", self.old)
        # The port's three patches, as commits of the work copy.
        work = self.port / "work" / "gridcraft"
        work.parent.mkdir()
        support.git(self.root, "clone", "--quiet", str(checkout), str(work))
        support.git(work, "checkout", "--quiet", "-b", "aros", self.old)
        support.commit(work, {"a.txt": "alpha\nport\n"}, "port: a line of our own")
        support.commit(work, {"b.txt": "one\ntwo port\nthree\n"}, "port: the second line our way")
        support.commit(work, {"c.txt": "shared\n"}, "port: a file upstream took later")
        series = self.port / "patches" / "gridcraft"
        support.git(work, "format-patch", "--quiet", "--no-numbered", "--no-signature", "--zero-commit", "-o", str(series), f"{self.old}..HEAD")
        self.names = sorted(p.name for p in series.glob("*.patch"))
        (series / "series").write_text("".join(n + "\n" for n in self.names))
        (work / "scratch.txt").write_text("not committed\n")
        crates = self.port / "patches" / "crates"
        crates.mkdir()
        (crates / "crates.json").write_text(json.dumps({"crates": [{"name": "foo", "version": "1.0.0"}, {"name": "libc", "version": "0.2.1"}]}))
        # Upstream moves on after the port's checkout was made.
        self.new = support.commit(self.upstream, {"b.txt": "one\ntwo upstream\nthree\n", "c.txt": "shared\n", "Cargo.lock": LOCK_NEW,
                                                  "Cargo.toml": MANIFEST.format(rev="b" * 40), "crates/d/Cargo.toml": "[package]\nname = \"d\"\n"}, "new")
        self.report = self.root / "report.json"

    def state(self):
        work = self.port / "work" / "gridcraft"
        checkout = self.port / "upstream" / "gridcraft"
        files = {p.relative_to(self.port).as_posix(): p.read_bytes() for p in sorted(self.port.rglob("*"))
                 if p.is_file() and ".git" not in p.parts and "build" not in p.relative_to(self.port).parts[:1]}
        return (files, support.git(work, "rev-parse", "HEAD"), support.git(work, "status", "--porcelain", "--untracked-files=all"),
                support.git(checkout, "rev-parse", "HEAD"), support.git(checkout, "status", "--porcelain", "--untracked-files=all"),
                support.git(checkout, "rev-list", "--all", "--count"))

    def check(self, commit, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = craft.main(self.port, ["update-check", "--upstream-commit", commit, "--output", str(self.report), *extra])
        return code, out.getvalue(), err.getvalue()

    def test_each_patch_gets_its_status_and_nothing_of_the_port_changes(self):
        before = self.state()
        code, _, err = self.check(self.new)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.state(), before)
        report = json.loads(self.report.read_text())
        self.assertEqual(report["current"]["commit"], self.old)
        self.assertEqual(report["candidate"]["commit"], self.new)
        status = {p["name"]: p["status"] for p in report["patches"]}
        self.assertEqual(status, {self.names[0]: "applies", self.names[1]: "conflicts", self.names[2]: "upstream-equivalent"})
        conflict = next(p for p in report["patches"] if p["status"] == "conflicts")
        self.assertEqual(conflict["files"], ["b.txt"])
        self.assertFalse(report["promoted"])
        self.assertEqual(report["work"], {"exists": True, "head": before[1], "dirty": True})

    def test_the_dependency_changes_are_listed(self):
        self.assertEqual(self.check(self.new)[0], 0)
        report = json.loads(self.report.read_text())
        self.assertEqual(report["lock"]["added"], {"bar": ["3.0.0"]})
        self.assertEqual(report["lock"]["changed"], {"foo": {"from": ["1.0.0"], "to": ["1.1.0"]}})
        self.assertEqual(report["lock"]["removed"], {})
        self.assertEqual(sorted(report["manifests"]["changed"]), ["Cargo.toml", "crates/d/Cargo.toml"])
        self.assertEqual(report["git_patches"]["changed"], {"egui": {"from": "a" * 40, "to": "b" * 40}})
        crates = {c["name"]: c for c in report["patched_crates"]}
        self.assertFalse(crates["foo"]["locked"])
        self.assertEqual(crates["foo"]["candidate_versions"], ["1.1.0"])
        self.assertTrue(crates["libc"]["locked"])

    def test_the_candidate_commit_is_fetched_apart_from_the_ports_checkout(self):
        checkout = self.port / "upstream" / "gridcraft"
        with self.assertRaises(AssertionError):
            support.git(checkout, "cat-file", "-e", self.new + "^{commit}")
        self.assertEqual(self.check(self.new)[0], 0)
        with self.assertRaises(AssertionError):
            support.git(checkout, "cat-file", "-e", self.new + "^{commit}")

    def test_the_ports_checkout_may_stand_at_another_commit(self):
        checkout = self.port / "upstream" / "gridcraft"
        support.git(checkout, "fetch", "--quiet", "origin")
        support.git(checkout, "-c", "advice.detachedHead=false", "checkout", "--quiet", "--detach", self.new)
        before = self.state()
        code, _, err = self.check(self.new)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.state(), before)
        self.assertEqual(json.loads(self.report.read_text())["summary"], {"applies": 1, "conflicts": 1, "upstream-equivalent": 1})

    def test_a_name_that_can_move_is_refused(self):
        code, _, err = self.check("main")
        self.assertEqual(code, 1)
        self.assertIn("40", err)
        self.assertFalse(self.report.exists())

    def test_an_unknown_commit_is_an_error_and_leaves_no_report(self):
        before = self.state()
        code, _, err = self.check("f" * 40)
        self.assertEqual(code, 1)
        self.assertFalse(self.report.exists())
        self.assertEqual(self.state(), before)

    def test_the_trial_can_be_kept_for_resolving_conflicts(self):
        keep = self.root / "trial here"
        self.assertEqual(self.check(self.new, "--keep", str(keep))[0], 0)
        self.assertEqual((keep / "a.txt").read_text(), "alpha\nport\n")
        self.assertEqual((keep / "b.txt").read_text(), "one\ntwo upstream\nthree\n", "a conflicting patch is left out, not half applied")
        self.assertEqual(support.git(keep, "status", "--porcelain"), "")

    def test_an_existing_directory_is_never_taken_over(self):
        keep = self.root / "occupied"
        keep.mkdir()
        (keep / "mine.txt").write_text("keep me\n")
        code, _, err = self.check(self.new, "--keep", str(keep))
        self.assertEqual(code, 1)
        self.assertEqual((keep / "mine.txt").read_text(), "keep me\n")


if __name__ == "__main__":
    unittest.main()
