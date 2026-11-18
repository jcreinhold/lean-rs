"""Check added Rust lines against rustfmt.toml's max_width."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import tomllib


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", help="Git revision to compare with the working tree")
    base = parser.parse_args().base
    if base and not base.strip("0"):
        # GitHub uses an all-zero before SHA for the first push to a branch.
        base = git("hash-object", "-t", "tree", "-w", os.devnull).strip()
    else:
        base = git("rev-parse", "--verify", "--end-of-options", f"{base}^{{tree}}").strip()

    with Path("rustfmt.toml").open("rb") as config:
        max_width = tomllib.load(config)["max_width"]
    patch = git(
        "-c", "core.quotePath=false", "diff", "--no-ext-diff", "--no-textconv", "--no-color",
        "--no-prefix", "--find-renames", "--unified=0", "--inter-hunk-context=0", "--diff-filter=AMR",
        base, "--", "*.rs",
    )

    failed = False
    path = ""
    line_no = None
    for line in patch.split("\n"):
        if line.startswith("diff --git "):
            line_no = None
        elif line_no is None and line.startswith("+++ "):
            path = line[4:].removesuffix("\t")
        elif match := re.match(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@", line):
            line_no = int(match[1])
        elif line_no is not None and line.startswith("+"):
            width = len(line[1:].removesuffix("\r"))
            if width > max_width:
                print(f"{path}:{line_no}: {width} columns (limit {max_width})")
                failed = True
            line_no += 1
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
