#!/usr/bin/env python3
"""Generate THIRD-PARTY-NOTICES.md: the licences of everything it-matters ships inside its binary.

  python3 scripts/update-third-party-notices.py          # (re)write THIRD-PARTY-NOTICES.md
  python3 scripts/update-third-party-notices.py --check  # exit 1 if the file is out of date

Covers every Rust crate the `it-matters` daemon is built from (the normal dependency graph for
Linux; dev and build-script-only crates are not linked into the binary), with its licence text.
Run it again whenever dependencies change (Cargo.toml / Cargo.lock). The output is deterministic.
"""

import hashlib
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "THIRD-PARTY-NOTICES.md"
TARGET = "x86_64-unknown-linux-gnu"
DAEMON = "it-matters"
LICENSE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE", "COPYRIGHT")


def fail(msg):
    sys.exit(f"update-third-party-notices: {msg}")


def read_text(path):
    return path.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n").strip("\n")


def license_files(directory):
    """Top-level licence-like files of a package directory, in a stable order."""
    files = []
    for entry in sorted(directory.iterdir()):
        if entry.is_file() and entry.name.upper().startswith(LICENSE_PREFIXES):
            if entry.suffix.lower() in (".rs", ".toml", ".cfg", ".py", ".sh", ".json"):
                continue
            files.append(entry)
    return files


# ---------------------------------------------------------------------------------------------
# Rust crates
# ---------------------------------------------------------------------------------------------


def runtime_crates():
    meta = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET],
            cwd=ROOT,
        )
    )
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == DAEMON)
    seen, stack = set(), [root]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            # kind None = a normal dependency (linked in); "build" / "dev" are not shipped.
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    seen.discard(root)
    # Our own workspace crates (path dependencies) are not third-party.
    seen = {p for p in seen if packages[p].get("source")}
    return sorted((packages[p] for p in seen), key=lambda p: (p["name"], p["version"]))


def crate_entries(crates):
    """(licence expression, [(name, version, repository)], text) grouped by identical text."""
    groups = {}  # text hash -> {"text": str, "crates": [...], "spdx": set}
    missing = []
    for crate in crates:
        directory = Path(crate["manifest_path"]).parent
        spdx = crate.get("license") or "see licence file"
        texts = [(f.name, read_text(f)) for f in license_files(directory)]
        if not texts:
            missing.append(f'{crate["name"]} {crate["version"]} ({spdx})')
            texts = [("", f"(No licence file is included in this crate's package; it is licensed under: {spdx}.)")]
        for _name, text in texts:
            key = hashlib.sha256(text.encode()).hexdigest()
            g = groups.setdefault(key, {"text": text, "crates": [], "spdx": set()})
            entry = (crate["name"], crate["version"], crate.get("repository") or "")
            if entry not in g["crates"]:
                g["crates"].append(entry)
            g["spdx"].add(spdx)
    return groups, missing


# ---------------------------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------------------------

HEADER = """# Third-party notices

it-matters itself is MIT licensed (see [LICENSE](LICENSE)). The it-matters binary contains the
following third-party software, whose licences are reproduced below as they require.

This file is generated: run `python3 scripts/update-third-party-notices.py` after changing
dependencies (Cargo.toml / Cargo.lock) and commit the result.

"""


def render():
    crates = runtime_crates()
    groups, missing = crate_entries(crates)

    out = [HEADER]
    out.append("## Rust crates\n")
    out.append(f"{len(crates)} crates are compiled into the binary. Crates sharing a licence text are listed together.\n")
    ordered = sorted(groups.values(), key=lambda g: (sorted(g["spdx"]), g["crates"][0][0], g["crates"][0][1]))
    for n, g in enumerate(ordered, 1):
        spdx = " / ".join(sorted(g["spdx"]))
        out.append(f"### Licence text {n}: {spdx}\n")
        out.append("Used by:\n")
        for name, version, repo in g["crates"]:
            suffix = f" ({repo})" if repo else ""
            out.append(f"- {name} {version}{suffix}")
        out.append("\n```text\n" + g["text"] + "\n```\n")
    text = "\n".join(out).rstrip("\n") + "\n"
    return text, missing


def main():
    check = "--check" in sys.argv[1:]
    text, missing = render()
    if missing:
        print("note: these crates ship no licence file (their SPDX expression is recorded instead):", file=sys.stderr)
        for m in missing:
            print(f"  - {m}", file=sys.stderr)
    if check:
        current = OUT.read_text(encoding="utf-8") if OUT.is_file() else ""
        if current != text:
            fail("THIRD-PARTY-NOTICES.md is out of date; run `python3 scripts/update-third-party-notices.py`.")
        print("THIRD-PARTY-NOTICES.md is up to date.")
        return
    OUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)} ({len(text) // 1024} KiB)")


if __name__ == "__main__":
    main()
