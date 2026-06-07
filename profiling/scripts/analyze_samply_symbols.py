#!/usr/bin/env python3
"""Summarize whether a saved samply Firefox profile has useful symbols."""

from __future__ import annotations

import gzip
import json
import re
import sys
from pathlib import Path


RAW_ADDR = re.compile(r"^0x[0-9a-fA-F]+$")


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: analyze_samply_symbols.py <profile.json.gz>", file=sys.stderr)
        return 2

    path = Path(sys.argv[1])
    try:
        with gzip.open(path, "rt") as handle:
            profile = json.load(handle)
    except OSError as exc:
        print(f"could not read {path}: {exc}", file=sys.stderr)
        return 1

    total = 0
    raw = 0
    lean_threads = 0
    named_examples: list[str] = []

    for thread in profile.get("threads", []):
        if "lean" in str(thread.get("name", "")).lower():
            lean_threads += 1
        strings = thread.get("stringArray", [])
        func_names = thread.get("funcTable", {}).get("name", [])
        for name_index in func_names:
            if not isinstance(name_index, int) or name_index < 0 or name_index >= len(strings):
                continue
            name = str(strings[name_index])
            total += 1
            if RAW_ADDR.match(name):
                raw += 1
            elif len(named_examples) < 5:
                named_examples.append(name)

    if total == 0:
        print("profile captured; no function symbols found; open in Firefox Profiler")
        return 0

    raw_pct = raw / total
    if raw_pct > 0.75:
        print(
            "profile captured but mostly not symbolicated "
            f"({raw}/{total} raw address symbols, lean_threads={lean_threads}); "
            "open in Firefox Profiler or rebuild with fuller native symbols"
        )
    else:
        examples = ", ".join(named_examples) if named_examples else "none"
        print(
            "profile captured with usable symbols "
            f"({total - raw}/{total} named, lean_threads={lean_threads}); examples: {examples}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
