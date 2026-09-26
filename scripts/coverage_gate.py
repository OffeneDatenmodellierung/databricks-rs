#!/usr/bin/env python3
"""Per-file coverage ratchet.

Reads tarpaulin's JSON report and fails if any hand-written library source
file (crates/*/src/**) is below the threshold (default 85%). The xtask
generator is checked end to end instead (`cargo xtask codegen --check` plus
tests/generated_patterns.rs).
Generated files (first line `// Code generated`) are exempt: they are
covered by the generator's own tests and tests/generated_patterns.rs.

    cargo tarpaulin --workspace --out Json --output-dir target/coverage \
        --exclude-files 'crates/*/examples/*' --exclude-files 'crates/*/build.rs' \
        --exclude-files 'crates/*/tests/*'
    python3 scripts/coverage_gate.py target/coverage/tarpaulin-report.json
"""

import json
import os
import sys

THRESHOLD = float(os.environ.get("COVERAGE_THRESHOLD", "85"))


def main(report: str) -> int:
    data = json.load(open(report))
    rows, failures = [], []
    for f in data["files"]:
        # tarpaulin has emitted the path both as components and as a string.
        parts = f["path"]
        if isinstance(parts, str):
            parts = [p for p in parts.replace("\\", "/").split("/") if p]
        anchor = next((i for i, p in enumerate(parts) if p in ("crates", "xtask")), None)
        path = "/".join(parts[anchor:]) if anchor is not None else "/".join(parts)
        if not path.startswith("crates/") or "/src/" not in path or not f["coverable"]:
            continue
        if f.get("content", "").startswith("// Code generated"):
            continue
        pct = 100.0 * f["covered"] / f["coverable"]
        rows.append((path, f["covered"], f["coverable"], pct))
        if pct < THRESHOLD:
            failures.append(path)
    for path, cov, tot, pct in sorted(rows):
        flag = "  FAIL" if path in failures else ""
        print(f"{pct:6.1f}%  {cov:4}/{tot:<4}  {path}{flag}")
    cov = sum(r[1] for r in rows)
    tot = sum(r[2] for r in rows) or 1
    print(f"\nhand-written total {100.0 * cov / tot:.2f}%  (per-file gate {THRESHOLD:.0f}%)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "target/coverage/tarpaulin-report.json"))
