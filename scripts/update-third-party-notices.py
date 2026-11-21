#!/usr/bin/env python3
"""Generate THIRD-PARTY-NOTICES.md: the licences of everything it-matters ships inside its binary.

  python3 scripts/update-third-party-notices.py          # (re)write THIRD-PARTY-NOTICES.md
  python3 scripts/update-third-party-notices.py --check  # exit 1 if the file is out of date

Covers every Rust crate the `it-matters` daemon and the bundled plugin binaries are built from (the normal dependency graph for
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
# Every binary the project ships: the daemon and the bundled plugins.
BINARIES = ["it-matters", "it-matters-plugin-linux-system", "it-matters-plugin-linux-audio"]
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


def shipped_names():
    """`(name, version)` of every third-party crate compiled into any shipped binary.

    Uses `cargo tree -e normal`, which (unlike the resolve graph of `cargo metadata`) does not unify the
    features that dev-dependencies enable into the normal build, so test-only crates are not listed."""
    out = set()
    for binary in BINARIES:
        text = subprocess.check_output(
            ["cargo", "tree", "-p", binary, "-e", "normal", "--target", TARGET, "--prefix", "none",
             "--format", "{p}", "--locked"],
            cwd=ROOT, text=True,
        )
        for line in text.splitlines():
            parts = line.replace(" (*)", "").replace(" (proc-macro)", "").split()
            # "name v1.2.3" for registry and git crates; workspace crates carry a "(/path)" suffix.
            if len(parts) >= 2 and parts[1].startswith("v") and not any(p.startswith("(/") for p in parts[2:]):
                out.add((parts[0], parts[1][1:]))
    return out


def runtime_crates():
    meta = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET],
            cwd=ROOT,
        )
    )
    wanted = shipped_names()
    found = [p for p in meta["packages"] if (p["name"], p["version"]) in wanted and p.get("source")]
    missing = wanted - {(p["name"], p["version"]) for p in found}
    if missing:
        fail(f"cargo tree lists crates that cargo metadata does not know: {sorted(missing)}")
    return sorted(found, key=lambda p: (p["name"], p["version"]))


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

it-matters itself is MIT licensed (see [LICENSE](LICENSE)). The it-matters binaries contain the
following third-party software, whose licences are reproduced below as they require.

This file is generated: run `python3 scripts/update-third-party-notices.py` after changing
dependencies (Cargo.toml / Cargo.lock) and commit the result.

"""


def render():
    crates = runtime_crates()
    groups, missing = crate_entries(crates)

    out = [HEADER]
    out.append("## Rust crates\n")
    out.append(f"{len(crates)} crates are compiled into the binaries. Crates sharing a licence text are listed together.\n")
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
