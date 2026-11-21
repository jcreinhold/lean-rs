"""Check the fallback Lean version in the CI job's full-matrix expression."""

import argparse
import json
import re
from pathlib import Path


def named_block(lines: list[str], indent: int, name: str) -> tuple[str, list[str]]:
    header = re.compile(rf"^{' ' * indent}{name}:(?: +(.*))?$")
    matches = [(i, match) for i, line in enumerate(lines) if (match := header.fullmatch(line))]
    if len(matches) != 1:
        raise ValueError(f"expected exactly one {name!r} key at indentation {indent}")

    start, match = matches[0]
    body = []
    for line in lines[start + 1 :]:
        if not line.strip():
            continue
        if line.startswith(" " * (indent + 1)):
            body.append(line)
        elif not line.lstrip().startswith("#"):
            break
    return (match[1] or "").strip(), body


def check_default(workflow: str, expected: str) -> None:
    lines = workflow.splitlines()
    for indent, name in enumerate(("jobs", "check", "strategy", "matrix")):
        value, lines = named_block(lines, indent * 2, name)
        if value:
            raise ValueError(f"expected {name!r} to be a block mapping")

    style, lines = named_block(lines, 8, "lean_version")
    expression = re.fullmatch(
        r"\$\{\{\s*fromJSON\(\s*inputs\.full_matrix\s*&&\s*'[^']*'\s*\|\|\s*'([^']*)'\s*\)\s*\}\}",
        " ".join(line.strip() for line in lines),
    )
    if style != ">-" or expression is None:
        raise ValueError(
            "expected a folded fromJSON(inputs.full_matrix && … || …) for lean_version"
        )
    actual = json.loads(expression[1])
    if actual != [expected]:
        raise ValueError(f"default Lean matrix is {actual!r}, expected {[expected]!r}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workflow", type=Path)
    parser.add_argument("head", help="head version from SUPPORTED_TOOLCHAINS")
    args = parser.parse_args()
    try:
        check_default(args.workflow.read_text(encoding="utf-8"), args.head)
    except (OSError, ValueError) as error:
        parser.exit(1, f"{args.workflow}: {error}\n")


if __name__ == "__main__":
    main()
