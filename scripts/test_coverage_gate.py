import json
import os
import tempfile
import unittest
from contextlib import redirect_stdout
from io import StringIO

import coverage_gate


def report(path):
    return {"files": [{"path": path, "covered": 9, "coverable": 10, "content": ""}]}


class PathShapes(unittest.TestCase):
    def run_gate(self, path):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(report(path), f)
        out = StringIO()
        try:
            with redirect_stdout(out):
                rc = coverage_gate.main(f.name)
        finally:
            os.unlink(f.name)
        return rc, out.getvalue()

    def test_component_list(self):
        rc, out = self.run_gate(["/", "w", "crates", "c", "src", "lib.rs"])
        self.assertEqual(rc, 0)
        self.assertIn("crates/c/src/lib.rs", out)

    def test_string_path(self):
        rc, out = self.run_gate("/w/crates/c/src/lib.rs")
        self.assertEqual(rc, 0)
        self.assertIn("crates/c/src/lib.rs", out)


if __name__ == "__main__":
    unittest.main()
