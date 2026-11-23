"""Files a port puts in the program drawer besides the programs (release.json drawer_files)."""
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import craft  # noqa: E402,F401  (sets up the module paths package.py uses)
import package  # noqa: E402


class DrawerFiles(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.packaging = Path(self.tmp.name) / "packaging"
        self.packaging.mkdir()
        (self.packaging / "App.portable").write_bytes(b"")
        (Path(self.tmp.name) / "outside.txt").write_text("x")
        self.saved, package.PACKAGING = package.PACKAGING, self.packaging

    def tearDown(self):
        package.PACKAGING = self.saved
        self.tmp.cleanup()

    def test_a_named_file_goes_beside_the_programs(self):
        files = package.drawer_files({"drawer_files": {"App.portable": "App.portable"}}, "App")
        self.assertEqual(files, {"App/App.portable": (b"", 0o100644)})

    def test_no_entry_adds_nothing(self):
        self.assertEqual(package.drawer_files({}, "App"), {})

    def test_names_with_paths_and_files_outside_packaging_are_refused(self):
        for entry in ({"sub/App.portable": "App.portable"}, {".hidden": "App.portable"}, {"Data:x": "App.portable"},
                      {"outside.txt": "../outside.txt"}, {"missing": "missing"}):
            with self.assertRaises(package.Failure, msg=str(entry)):
                package.drawer_files({"drawer_files": entry}, "App")


if __name__ == "__main__":
    unittest.main()
