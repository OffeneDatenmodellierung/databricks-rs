#!/usr/bin/env python3
"""Compare a docs sample (method + path per page) with spec/openapi/*.json
and write spec/DOCS-CHECK.md. Path parameter names are normalised to {}."""

import glob
import json
import os
import re

here = os.path.dirname(os.path.abspath(__file__))
spec = os.path.dirname(here)
norm = lambda p: re.sub(r"\{[^}]+\}", "{}", p)

ops = {}
for client in ("account", "workspace"):
    d = json.load(open(os.path.join(spec, "openapi", f"{client}.json")))
    ops[client] = {}
    for path, item in d["paths"].items():
        for verb, op in item.items():
            if verb.startswith("x-"):
                continue
            ops[client][f"{verb.upper()} {norm(path)}"] = op["operationId"]
    version = d["info"]["version"], d["info"]["x-databricks-openapi-sha"]

sample_path = sorted(glob.glob(os.path.join(here, "sample-*.json")))[-1]
sample = json.load(open(sample_path))
rows, ok, bad, weak_bad = [], 0, 0, 0
for page in sample["pages"]:
    for op in page["ops"]:
        verb, path = op.split(" ", 1)
        key = f"{verb} {norm(path)}"
        hit = ops[page["client"]].get(key)
        if hit:
            ok += 1
        elif page.get("weak"):
            weak_bad += 1
        else:
            bad += 1
        status = f"✅ `{hit}`" if hit else ("⚠️ weak evidence, no match" if page.get("weak") else "❌ not in spec")
        rows.append(f"| {page['client']} | `{page['page']}` | `{op}` | {status} |")

out = [
    "# Spot-check against the published API reference",
    "",
    f"Spec: databricks-sdk-go v{version[0]}, OpenAPI `{version[1]}`. Sample: `{os.path.basename(sample_path)}` ({sample['fetched']}).",
    "",
    f"> {sample['method']}",
    "",
    f"**{ok} matched, {bad} missing, {weak_bad} weak-evidence mismatches.**",
    "",
    "| Client | Docs page | Documented operation | In our spec |",
    "|---|---|---|---|",
    *rows,
    "",
    "Regenerate with `python3 spec/docs-check/compare.py` after adding a new `sample-<date>.json`.",
    "",
]
open(os.path.join(spec, "DOCS-CHECK.md"), "w").write("\n".join(out))
print(f"matched={ok} missing={bad} weak={weak_bad}")
