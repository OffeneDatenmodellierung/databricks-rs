#!/usr/bin/env python3
"""Dependency age policy: use the latest release of every crate, except
releases published in the last 48 hours.

Two modes:

    # Fail if Cargo.lock contains any crates.io package published < 48h ago.
    python3 scripts/check_dep_age.py check [Cargo.lock]

    # Print the newest acceptable version of each named crate (for bumping
    # [workspace.dependencies]).
    python3 scripts/check_dep_age.py latest serde tokio reqwest

To bring a too-new locked crate back into policy:

    cargo update -p <name>@<new> --precise <version printed by `latest`>

When a group of crates pins each other exactly (wasm-bindgen / js-sys /
web-sys), downgrade them together by temporarily adding `=` requirements
under a `[target.'cfg(target_arch = "wasm32")'.dev-dependencies]` table,
running `cargo update -p …` for the group, then removing the table.
"""

import datetime
import json
import re
import sys
import time
import tomllib
import urllib.request

UA = "databricks-rs dep-age-check (github.com/OffeneDatenmodellierung/databricks-rs)"
WINDOW = datetime.timedelta(hours=48)


def _get(url: str) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    return json.load(urllib.request.urlopen(req))


def _ts(s: str) -> datetime.datetime:
    return datetime.datetime.fromisoformat(s.replace("Z", "+00:00"))


def _key(num: str):
    return tuple(int(x) for x in re.split(r"[.+-]", num)[:3])


def check(lock_path: str) -> int:
    cutoff = datetime.datetime.now(datetime.timezone.utc) - WINDOW
    lock = tomllib.load(open(lock_path, "rb"))
    pkgs = [p for p in lock["package"] if p.get("source", "").startswith("registry+")]
    too_new = []
    for p in pkgs:
        v = _get(f"https://crates.io/api/v1/crates/{p['name']}/{p['version']}")["version"]
        if _ts(v["created_at"]) > cutoff:
            too_new.append((p["name"], p["version"], v["created_at"][:16]))
        time.sleep(0.1)  # crates.io asks for <= 1 req/s sustained; bursts are fine
    print(f"checked {len(pkgs)} crates.io packages against a {WINDOW} window")
    for name, ver, when in too_new:
        print(f"  TOO NEW  {name} {ver}  published {when}Z")
    return 1 if too_new else 0


def latest(names: list[str]) -> int:
    cutoff = datetime.datetime.now(datetime.timezone.utc) - WINDOW
    for name in names:
        versions = _get(f"https://crates.io/api/v1/crates/{name}/versions?per_page=100")["versions"]
        stable = sorted(
            (v for v in versions if not v["yanked"] and "-" not in v["num"]),
            key=lambda v: _key(v["num"]),
            reverse=True,
        )
        ok = next((v for v in stable if _ts(v["created_at"]) < cutoff), None)
        newest = stable[0]["num"] if stable else "?"
        note = "" if ok and ok["num"] == newest else f"  (newest {newest} is inside the window)"
        print(f"{name:24} {ok['num'] if ok else '-':10} {ok['created_at'][:10] if ok else ''}{note}")
    return 0


if __name__ == "__main__":
    args = sys.argv[1:] or ["check"]
    if args[0] == "check":
        sys.exit(check(args[1] if len(args) > 1 else "Cargo.lock"))
    if args[0] == "latest":
        sys.exit(latest(args[1:]))
    sys.exit(__doc__)
