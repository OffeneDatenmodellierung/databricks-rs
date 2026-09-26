#!/usr/bin/env python3
"""Publish workspace crates that are not on crates.io yet, in dependency order.

crates.io rate-limits the creation of *new* crates: after a small burst it
answers `429 Too Many Requests: You have published too many new crates in a
short period of time. Please try again after <RFC 1123 date>`. This script
waits until that time (plus a minute for clock skew) and retries, instead of
guessing the limit.

It is idempotent: a crate whose current version is already on crates.io is
skipped, so a run that stops part-way can simply be started again.

    publish_new_crates.py                 publish what is missing
    publish_new_crates.py --dry-run       show the plan
    publish_new_crates.py --check         exit 1 if any crate is missing
    publish_new_crates.py --max-minutes N stop cleanly after N minutes

With --max-minutes the script stops before starting a wait that would run
past the budget and reports the crates left in `remaining=<n>` (also
written to $GITHUB_OUTPUT), so a CI job can re-dispatch itself.
"""

from __future__ import annotations

import argparse
import email.utils
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

CRATES_IO = "https://crates.io/api/v1/crates"
USER_AGENT = "community-databricks-sdk publish script (github.com/OffeneDatenmodellierung/databricks-rs)"
RETRY_AFTER = re.compile(r"try again after ([A-Z][a-z]{2}, \d{1,2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} GMT)")
SKEW_SECONDS = 60


def workspace_packages() -> list[dict]:
    out = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], text=True
    )
    meta = json.loads(out)
    members = set(meta["workspace_members"])
    return [p for p in meta["packages"] if p["id"] in members]


def publishable(pkg: dict) -> bool:
    # `publish = false` shows up as an empty registry list.
    return pkg.get("publish") is None or pkg["publish"] != []


def publish_order(packages: list[dict]) -> list[dict]:
    """Topological order over normal and build dependencies between the
    workspace crates (dev-dependencies are stripped on publish)."""
    by_name = {p["name"]: p for p in packages}
    deps = {
        p["name"]: sorted(
            {
                d["name"]
                for d in p["dependencies"]
                if d["name"] in by_name and d.get("kind") in (None, "build")
            }
        )
        for p in packages
    }
    order: list[str] = []
    state: dict[str, str] = {}

    def visit(name: str) -> None:
        if state.get(name) == "done":
            return
        if state.get(name) == "active":
            raise SystemExit(f"dependency cycle through {name}")
        state[name] = "active"
        for d in deps[name]:
            visit(d)
        state[name] = "done"
        order.append(name)

    for name in sorted(by_name):
        visit(name)
    return [by_name[n] for n in order]


def on_crates_io(name: str, version: str) -> bool:
    req = urllib.request.Request(f"{CRATES_IO}/{name}/{version}", headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status == 200
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return False
        raise


def retry_after(output: str, now: datetime) -> float | None:
    """Seconds to wait if `output` is crates.io's new-crate rate-limit error."""
    m = RETRY_AFTER.search(output)
    if not m:
        return None
    when = email.utils.parsedate_to_datetime(m.group(1))
    return max(0.0, (when - now).total_seconds()) + SKEW_SECONDS


def publish(name: str) -> subprocess.CompletedProcess:
    # The workspace was built and packaged by CI; verifying each crate again
    # here would rebuild the tree 41 times.
    cmd = ["cargo", "publish", "-p", name, "--locked", "--no-verify"]
    return subprocess.run(cmd, capture_output=True, text=True)


def write_output(remaining: int) -> None:
    print(f"remaining={remaining}")
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(f"remaining={remaining}\n")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--max-minutes", type=float, default=None)
    args = ap.parse_args(argv)

    deadline = time.monotonic() + args.max_minutes * 60 if args.max_minutes else None
    packages = [p for p in publish_order(workspace_packages()) if publishable(p)]
    missing = [p for p in packages if not on_crates_io(p["name"], p["version"])]

    for p in packages:
        mark = "publish" if p in missing else "present"
        print(f"  {mark:8} {p['name']} {p['version']}")
    if args.check:
        print(f"{len(missing)} of {len(packages)} crates not on crates.io")
        return 1 if missing else 0
    if args.dry_run:
        write_output(len(missing))
        return 0

    for i, p in enumerate(missing):
        name = p["name"]
        while True:
            print(f"publishing {name} {p['version']}", flush=True)
            r = publish(name)
            if r.returncode == 0:
                break
            output = r.stdout + r.stderr
            if "already exists" in output or "is already uploaded" in output:
                print(f"  {name} {p['version']} is already on crates.io")
                break
            wait = retry_after(output, datetime.now(timezone.utc))
            if wait is None:
                sys.stderr.write(output)
                return 1
            if deadline is not None and time.monotonic() + wait > deadline:
                print(f"rate limited; the next slot is beyond this run's budget. Stopping with {name} next.")
                write_output(len(missing) - i)
                return 0
            print(f"  rate limited by crates.io; waiting {wait / 60:.1f} minutes", flush=True)
            time.sleep(wait)
    write_output(0)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
