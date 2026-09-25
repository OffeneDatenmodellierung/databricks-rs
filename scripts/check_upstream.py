#!/usr/bin/env python3
"""Which Databricks OpenAPI spec is current, and are we on it?

The OpenAPI spec itself is not published. Each official SDK pins the spec
it was generated from in `.codegen/_openapi_sha`. This script reads that
pin at the latest release tag of each SDK and compares it to ours
(`spec/ir.json`), applying the house 48-hour rule to releases.

    python3 scripts/check_upstream.py            # table
    python3 scripts/check_upstream.py --github   # also write GITHUB_OUTPUT

Exit status 0 always; `update=true` is written to GITHUB_OUTPUT when a
newer databricks-sdk-go release (older than 48h) is available.
"""

import datetime
import json
import os
import re
import subprocess
import sys
import tempfile

REPOS = ["databricks-sdk-go", "databricks-sdk-py", "databricks-sdk-java", "cli"]
WINDOW = datetime.timedelta(hours=48)


def git(*args, cwd=None):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout


def latest_tags(repo):
    out = git("ls-remote", "--tags", f"https://github.com/databricks/{repo}")
    tags = {line.split("refs/tags/")[1] for line in out.splitlines() if "^{}" not in line}
    tags = [t for t in tags if re.fullmatch(r"v\d+\.\d+\.\d+", t)]
    return sorted(tags, key=lambda t: tuple(int(x) for x in t[1:].split(".")), reverse=True)


def release_info(repo, tag, tmp):
    d = os.path.join(tmp, repo + tag)
    git("clone", "-q", "--depth", "1", "--branch", tag, "--filter=blob:none", "--sparse",
        f"https://github.com/databricks/{repo}", d)
    git("sparse-checkout", "set", ".codegen", cwd=d)
    sha = open(os.path.join(d, ".codegen/_openapi_sha")).read().strip()
    when = git("log", "-1", "--format=%cI", cwd=d).strip()
    return sha, datetime.datetime.fromisoformat(when)


def main():
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    ours = json.load(open(os.path.join(here, "spec/ir.json")))["source"]
    now = datetime.datetime.now(datetime.timezone.utc)
    print(f"ours: databricks-sdk-go {ours['go_sdk_version']}  openapi {ours['openapi_sha']}\n")
    print(f"{'repo':22}{'latest':10}{'released':12}{'openapi sha':42}{'>48h':6}")
    newest_go = None
    with tempfile.TemporaryDirectory() as tmp:
        for repo in REPOS:
            for tag in latest_tags(repo)[:3]:
                sha, when = release_info(repo, tag, tmp)
                old_enough = now - when > WINDOW
                print(f"{repo:22}{tag:10}{when.date().isoformat():12}{sha:42}{'yes' if old_enough else 'no':6}")
                if repo == "databricks-sdk-go" and old_enough and newest_go is None:
                    newest_go = (tag, sha)
                if old_enough:
                    break
    update = newest_go is not None and newest_go[0] != ours["go_sdk_version"]
    print(f"\nnewest eligible databricks-sdk-go: {newest_go[0] if newest_go else '-'}; update needed: {update}")
    if "--github" in sys.argv and os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as f:
            f.write(f"update={'true' if update else 'false'}\n")
            if newest_go:
                f.write(f"go_sdk_tag={newest_go[0]}\nopenapi_sha={newest_go[1]}\n")


if __name__ == "__main__":
    main()
