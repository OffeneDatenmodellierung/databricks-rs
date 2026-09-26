#!/usr/bin/env python3
"""Surface parity with databricks-sdk-go.

Compares the Go SDK checkout pinned by spec/ir.json with this SDK and
fails on any Go surface that is neither generated, covered by a rule, nor
listed in codegen/parity.toml:

* client accessors (workspace_client.go / account_client.go fields);
* service methods (api.go interfaces), after the rules below;
* long-running operations (XOperationInterface), which must be in the IR;
* hand-written helpers (service/*/ext_*.go, and methods on the clients).

Rules for service methods the IR doesn't list, because this SDK covers
them another way:

* ``Wait…``          generated waiters (``wait_…``);
* ``…AndWait``       the generated call returns a waiter (``.await?``);
* ``…All``           generated ``…_all`` methods;
* ``<Op>By<Field>``  positional shortcuts for ``<Op>``; request
                     constructors (``GetFooRequest::new(x)``) cover these.
                     ``GetBy…`` helpers that list and search instead are
                     not covered by this rule;
* ``X() XInterface`` sub-service accessors (``w.Settings.X()``), which are
                     top-level accessors here.

Entries in parity.toml are keyed by Go symbol (fnmatch patterns allowed)
with ``status`` one of ``implemented`` (``rust`` names ``path#fn``, which
must exist), ``gap`` (``issue``) or ``na`` (``reason``). Entries that no
longer match anything in Go are errors too, so the file can't go stale.

Writes spec/PARITY.md. Usage: check_parity.py GO_SDK_DIR [--root DIR]
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import re
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

RULES = {
    "lookup": "generated name lookup (`…_map`, `get_by_…`)",
    "wait": "generated waiter (`wait_…`)",
    "and_wait": "the call returns a waiter (`.await?` on it)",
    "all": "generated `…_all`",
    "by": "request constructor (`…Request::new(…)`)",
    "sub_accessor": "top-level accessor",
}


@dataclass
class Report:
    covered: dict[str, list[str]] = field(default_factory=dict)  # rule -> symbols
    listed: dict[str, tuple[str, dict]] = field(default_factory=dict)  # symbol -> (key, entry)
    errors: list[str] = field(default_factory=list)
    counts: dict[str, int] = field(default_factory=dict)

    def cover(self, rule: str, sym: str) -> None:
        self.covered.setdefault(rule, []).append(sym)


def go_accessors(go: Path) -> dict[str, set[str]]:
    out = {}
    for client, f in (("workspace", "workspace_client.go"), ("account", "account_client.go")):
        src = (go / f).read_text()
        out[client] = set(re.findall(r"^\s+([A-Z]\w+) +\w+\.\w+(?:Interface|Service)\b", src, re.M))
    return out


def go_interfaces(go: Path) -> dict[tuple[str, str], list[tuple[str, str]]]:
    """(package, service) -> [(method, signature line)] from api.go."""
    out = {}
    for f in sorted(go.glob("service/*/api.go")):
        pkg = f.parent.name
        src = f.read_text()
        for m in re.finditer(r"^type (\w+)Interface interface \{(.*?)^\}", src, re.S | re.M):
            svc = m.group(1)
            if svc.endswith("Operation"):
                continue
            out[(pkg, svc)] = re.findall(r"^\t(([A-Z]\w*)\(.*)$", m.group(2), re.M)
            out[(pkg, svc)] = [(name, line) for line, name in out[(pkg, svc)]]
    return out


def go_list_lookups(go: Path) -> set[str]:
    """`GetByX` helpers that list everything and search (not positional
    shortcuts): their bodies call `ListAll` or a `…Map` helper."""
    out = set()
    for f in sorted(go.glob("service/*/api.go")):
        pkg = f.parent.name
        for m in re.finditer(
            r"^func \(a \*(\w+)API\) (\w*By\w*)\(ctx context\.Context, [^)]*\) [^{]*\{(.*?)^\}",
            f.read_text(),
            re.S | re.M,
        ):
            if "ListAll(" in m.group(3) or "Map(" in m.group(3):
                out.add(f"{pkg}.{m.group(1)}.{m.group(2)}")
    return out


def go_lro_methods(go: Path) -> set[str]:
    out = set()
    for f in sorted(go.glob("service/*/api.go")):
        pkg = f.parent.name
        for m in re.finditer(
            r"^func \(a \*(\w+)API\) (\w+)\(ctx context\.Context, request \w+\) \(\w+OperationInterface, error\)",
            f.read_text(),
            re.M,
        ):
            out.add(f"{pkg}.{m.group(1)}.{m.group(2)}")
    return out


def go_helpers(go: Path) -> set[str]:
    """Exported hand-written functions and methods on exported types."""
    out = set()
    files = [p for p in go.glob("service/*/ext_*.go") if not p.name.endswith("_test.go")]
    for f in sorted(files):
        pkg = f.parent.name
        for m in re.finditer(r"^func (?:\((\w+) \*?(\w+)\) )?([A-Z]\w*)\(", f.read_text(), re.M):
            recv, name = m.group(2), m.group(3)
            if recv is None:
                out.add(f"{pkg}.{name}")
            elif recv[0].isupper():
                out.add(f"{pkg}.{recv}.{name}")
    for f in sorted(go.glob("*.go")):
        if f.name.endswith("_test.go"):
            continue
        for m in re.finditer(
            r"^func \(\w+ \*((?:Workspace|Account)Client)\) ([A-Z]\w*)\(", f.read_text(), re.M
        ):
            out.add(f"{m.group(1)}.{m.group(2)}")
    return out


def classify(name: str, line: str, ir_methods: set[str]) -> str | None:
    if name.startswith("Wait"):
        return "wait"
    if name.endswith("AndWait"):
        return "and_wait"
    if name.endswith("All") and name[: -len("All")] in ir_methods:
        return "all"
    by = re.match(r"^([A-Z][a-z]\w*?)By[A-Z]", name)
    if by and by.group(1) in ir_methods:
        return "by"
    if re.match(rf"^{name}\(\) \w+Interface$", line.strip()):
        return "sub_accessor"
    return None


def specificity(pattern: str) -> tuple[int, int]:
    """More literal characters, then fewer wildcards, is more specific."""
    literal = len(re.sub(r"[*?]|\[[^]]*\]", "", pattern))
    return literal, -sum(pattern.count(c) for c in "*?[")


def lookup(entries: dict[str, dict], sym: str) -> tuple[str, dict] | None:
    """The exact entry, else the most specific matching pattern."""
    if sym in entries:
        return sym, entries[sym]
    hits = [k for k in entries if any(c in k for c in "*?[") and fnmatch.fnmatchcase(sym, k)]
    if not hits:
        return None
    key = max(hits, key=specificity)
    return key, entries[key]


def validate(key: str, e: dict, root: Path) -> list[str]:
    status = e.get("status")
    if status == "implemented":
        rust = e.get("rust", "")
        path, _, fn = rust.partition("#")
        f = root / path
        # `fn name`, or `= name` as an argument to a macro that defines it.
        pattern = rf"\bfn {re.escape(fn)}\b|=\s*{re.escape(fn)}\b"
        if not fn or not f.exists() or not re.search(pattern, f.read_text()):
            return [f"{key}: implemented, but `fn {fn}` not found in {path or '(no rust path)'}"]
        return []
    if status == "gap":
        return [] if isinstance(e.get("issue"), int) else [f"{key}: gap needs an issue number"]
    if status == "na":
        return [] if e.get("reason") else [f"{key}: na needs a reason"]
    return [f"{key}: unknown status {status!r}"]


def check(go: Path, root: Path) -> Report:
    ir = json.loads((root / "spec/ir.json").read_text())
    cfg = tomllib.loads((root / "codegen/parity.toml").read_text())
    accessors_t = cfg.get("accessors", {})
    methods_t = cfg.get("methods", {})
    helpers_t = cfg.get("helpers", {})
    r = Report()
    used: set[tuple[str, str]] = set()

    for table, entries in (("accessors", accessors_t), ("methods", methods_t), ("helpers", helpers_t)):
        for key, e in entries.items():
            r.errors += validate(f"[{table}] {key}", e, root)

    def listed(table: str, entries: dict, sym: str) -> bool:
        hit = lookup(entries, sym)
        if hit is None:
            return False
        used.add((table, hit[0]))
        r.listed[sym] = hit
        return True

    # Accessors.
    ours = {(s["client"], s["accessor"]) for s in ir["services"]}
    n = 0
    for client, names in go_accessors(go).items():
        for a in sorted(names):
            n += 1
            sym = f"{client}.{a}"
            if (client, a) not in ours and not listed("accessors", accessors_t, sym):
                r.errors.append(f"accessor {sym}: not generated and not in parity.toml")
    r.counts["accessors"] = n

    # Service methods.
    ir_methods = {(s["package"], s["name"]): {m["name"] for m in s.get("methods") or []} for s in ir["services"]}
    ir_lookups = {(s["package"], s["name"]): {l["name"] for l in s.get("lookups") or []} for s in ir["services"]}
    generated = sum(len(v) for v in ir_methods.values())
    unsupported = [
        f"{s['package']}.{s['name']}.{m['name']}"
        for s in ir["services"]
        for m in s.get("methods") or []
        if m.get("unsupported") and not m["unsupported"].startswith("binary")
    ]
    reachable = {(s["package"], s["name"]) for s in ir["services"]}
    reachable_pkgs = {p for p, _ in reachable}
    lookups = go_list_lookups(go)
    n = 0
    for (pkg, svc), methods in sorted(go_interfaces(go).items()):
        have = ir_methods.get((pkg, svc), set())
        for name, line in methods:
            if name in have:
                continue
            n += 1
            sym = f"{pkg}.{svc}.{name}"
            if name in ir_lookups.get((pkg, svc), set()):
                rule = "lookup"
            else:
                rule = None if sym in lookups else classify(name, line, have)
            if rule:
                r.cover(rule, sym)
            elif not listed("methods", methods_t, sym):
                where = "" if pkg in reachable_pkgs else " (package not on Go's clients)"
                r.errors.append(f"method {sym}: not generated, no rule, not in parity.toml{where}")
    for sym in unsupported:
        if not listed("methods", methods_t, sym):
            r.errors.append(f"method {sym}: extracted but not generated")
    r.counts["generated"] = generated
    r.counts["extra_go_methods"] = n

    # Long-running operations.
    ir_lro = {
        f"{s['package']}.{s['name']}.{m['name']}"
        for s in ir["services"]
        for m in s.get("methods") or []
        if m.get("lro")
    }
    go_lro = go_lro_methods(go)
    for sym in sorted(go_lro - ir_lro):
        r.errors.append(f"long-running {sym}: Go returns an operation handle; IR has no lro")
    r.counts["lro"] = len(go_lro)

    # Helpers.
    helpers = go_helpers(go)
    for sym in sorted(helpers):
        if not listed("helpers", helpers_t, sym):
            r.errors.append(f"helper {sym}: not in parity.toml")
    r.counts["helpers"] = len(helpers)

    for table, entries in (("accessors", accessors_t), ("methods", methods_t), ("helpers", helpers_t)):
        for key in entries:
            if (table, key) not in used:
                r.errors.append(f"parity.toml [{table}] {key}: matches nothing in Go (stale)")
    return r


def markdown(r: Report, version: str) -> str:
    lines = [
        "# Parity with databricks-sdk-go",
        "",
        f"Generated by `scripts/check_parity.py` against databricks-sdk-go {version}. Do not edit;",
        "change `codegen/parity.toml` or the code, then re-run.",
        "",
        "| | Count |",
        "|---|---|",
        f"| Client accessors in Go | {r.counts.get('accessors', 0)} |",
        f"| Generated operations | {r.counts.get('generated', 0)} |",
        f"| Long-running operations (typed handles) | {r.counts.get('lro', 0)} |",
        f"| Go service methods beyond the IR | {r.counts.get('extra_go_methods', 0)} |",
        f"| Hand-written Go helpers | {r.counts.get('helpers', 0)} |",
        "",
        "## Covered by rule",
        "",
        "| Go method kind | Here | Count |",
        "|---|---|---|",
    ]
    for rule, desc in RULES.items():
        lines.append(f"| {rule} | {desc} | {len(r.covered.get(rule, []))} |")
    for status, title in (("gap", "Gaps"), ("implemented", "Implemented by hand"), ("na", "Not applicable")):
        rows: dict[str, list[str]] = {}
        for sym, (key, e) in sorted(r.listed.items()):
            if e.get("status") == status:
                rows.setdefault(key, []).append(sym)
        lines += ["", f"## {title}", ""]
        if not rows:
            lines.append("None.")
            continue
        detail = {"gap": "Issue", "implemented": "Rust", "na": "Why"}[status]
        lines += [f"| Go | {detail} |", "|---|---|"]
        for key, syms in rows.items():
            e = r.listed[syms[0]][1]
            what = {"gap": f"#{e.get('issue')}", "implemented": f"`{e.get('rust')}`", "na": e.get("reason", "")}[status]
            name = f"`{key}`" if len(syms) == 1 and syms[0] == key else f"`{key}` ({len(syms)})"
            lines.append(f"| {name} | {what} |")
    lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("go_sdk", type=Path)
    ap.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = ap.parse_args(argv)
    r = check(args.go_sdk, args.root)
    version = json.loads((args.root / "spec/ir.json").read_text())["source"]["go_sdk_version"]
    (args.root / "spec/PARITY.md").write_text(markdown(r, version))
    for e in r.errors:
        print(f"error: {e}", file=sys.stderr)
    gaps = sum(1 for _, e in r.listed.values() if e.get("status") == "gap")
    print(
        f"parity: {r.counts.get('generated', 0)} operations, {r.counts.get('lro', 0)} long-running, "
        f"{sum(map(len, r.covered.values()))} by rule, {len(r.listed)} listed ({gaps} gaps), "
        f"{len(r.errors)} errors"
    )
    return 1 if r.errors else 0


if __name__ == "__main__":
    sys.exit(main())
