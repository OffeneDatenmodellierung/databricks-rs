"""Tests for publish_new_crates.py (run: python3 -m unittest discover scripts)."""

import sys
import unittest
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import publish_new_crates as pnc  # noqa: E402


def pkg(name, deps=(), publish=None, kind=None):
    return {
        "name": name,
        "version": "0.1.0",
        "publish": publish,
        "dependencies": [{"name": d, "kind": kind} for d in deps],
    }


class RetryAfter(unittest.TestCase):
    def test_parses_the_crates_io_message(self):
        out = (
            "error: failed to publish to registry at https://crates.io\n\nCaused by:\n"
            "  the remote server responded with an error (status 429 Too Many Requests): "
            "You have published too many new crates in a short period of time. "
            "Please try again after Fri, 04 Sep 2026 15:26:01 GMT and see "
            "https://crates.io/docs/rate-limits for more information on rate limits."
        )
        now = datetime(2026, 9, 4, 15, 16, 1, tzinfo=timezone.utc)
        self.assertEqual(pnc.retry_after(out, now), 600 + pnc.SKEW_SECONDS)

    def test_past_times_wait_only_for_skew(self):
        out = "Please try again after Fri, 04 Sep 2026 15:26:01 GMT"
        now = datetime(2026, 9, 5, tzinfo=timezone.utc)
        self.assertEqual(pnc.retry_after(out, now), pnc.SKEW_SECONDS)

    def test_other_errors_are_not_retried(self):
        self.assertIsNone(pnc.retry_after("error: 403 Forbidden", datetime.now(timezone.utc)))


class Order(unittest.TestCase):
    def test_dependencies_come_first(self):
        packages = [
            pkg("community-databricks-sdk", ["community-databricks-sdk-jobs", "community-databricks-core"]),
            pkg("community-databricks-sdk-jobs", ["community-databricks-sdk-compute", "community-databricks-core"]),
            pkg("community-databricks-sdk-compute", ["community-databricks-core", "serde"]),
            pkg("community-databricks-core", ["tokio"]),
        ]
        names = [p["name"] for p in pnc.publish_order(packages)]
        self.assertEqual(
            names,
            [
                "community-databricks-core",
                "community-databricks-sdk-compute",
                "community-databricks-sdk-jobs",
                "community-databricks-sdk",
            ],
        )

    def test_dev_dependencies_do_not_constrain_order(self):
        packages = [pkg("a", ["b"], kind="dev"), pkg("b", ["a"])]
        self.assertEqual([p["name"] for p in pnc.publish_order(packages)], ["a", "b"])

    def test_cycles_are_reported(self):
        with self.assertRaises(SystemExit):
            pnc.publish_order([pkg("a", ["b"]), pkg("b", ["a"])])

    def test_publish_false_is_skipped(self):
        self.assertFalse(pnc.publishable(pkg("xtask", publish=[])))
        self.assertTrue(pnc.publishable(pkg("a")))


class Check(unittest.TestCase):
    """--check gates the release job: an unreleased version of a published
    crate must pass (release-plz publishes it); a never-published crate fails."""

    def run_check(self, published):
        calls = []

        def on_crates_io(name, version=None):
            calls.append((name, version))
            return name in published

        old = (pnc.workspace_packages, pnc.on_crates_io)
        pnc.workspace_packages = lambda: [pkg("a"), pkg("b")]
        pnc.on_crates_io = on_crates_io
        try:
            return pnc.main(["--check"]), calls
        finally:
            pnc.workspace_packages, pnc.on_crates_io = old

    def test_published_crates_pass_whatever_their_version(self):
        code, calls = self.run_check({"a", "b"})
        self.assertEqual(code, 0)
        self.assertEqual(calls, [("a", None), ("b", None)])

    def test_a_never_published_crate_fails(self):
        code, _ = self.run_check({"a"})
        self.assertEqual(code, 1)


if __name__ == "__main__":
    unittest.main()
