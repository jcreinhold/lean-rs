#!/usr/bin/env python3
"""Run TLC on the worker custody model and compare each case with its expected result.

Good.cfg must pass. Each other configuration must fail the one property it
names; a configuration that passes means the property no longer checks the
rule it was written for.

A pass means TLC found no counterexample within the configured bounds. It is
not a proof about the running server.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent

# Configuration name -> the property TLC must report violated, or None to pass.
# README.md explains each failing configuration.
CASES = {
    "Good": None,
    "HistoricalLockOnly": "WriterExclusion",
    "StaleClosure": "NoEarlyCapacityReturn",
    "ExplicitUnlockLockOnly": "WriterExclusion",
    "DirectChildOnly": "BootstrapClosureIsSound",
    "PublishBeforeAdopt": "AdoptionPrecedesRunning",
    "AckBeforeDurable": "RunningPrecedesAck",
    "LoseAdoption": "AdoptionPrecedesRunning",
    "CollectPreparingOnParentDeath": "NoEarlyCapacityReturn",
    "ZombieReceiptBeforeCustodyClosed": "ZombieAdmissionFenced",
    "ZombieWriterWithoutReceipt": "ZombieAdmissionFenced",
    "DropPreparingAfterPossibleSpawn": "NoEarlyCapacityReturn",
    "DirectExitReturnsTrackerCapacity": "GuardianPresentImpliesCapacity",
}

VIOLATION = re.compile(
    r"Error: (?:Invariant (?P<invariant>\w+) is violated"
    r"|Action property (?P<action>\w+) is violated"
    r"|(?P<temporal>Temporal properties were violated))"
)


def find_java() -> str:
    for candidate in (
        os.environ.get("JAVA"),
        shutil.which("java"),
        "/opt/homebrew/opt/openjdk@21/bin/java",
    ):
        if candidate and Path(candidate).is_file():
            probe = subprocess.run([candidate, "-version"], capture_output=True)
            if probe.returncode == 0:
                return candidate
    sys.exit("No Java runtime found; set JAVA to a java executable")


def run_tlc(java: str, jar: Path, model: Path, config: Path) -> tuple[str, str]:
    """Return (outcome, log): "pass", the violated property, or "error"."""
    model, config = model.resolve(), config.resolve()
    with tempfile.TemporaryDirectory(prefix="tlc-") as work:
        result = subprocess.run(
            [java, "-XX:+UseParallelGC", "-cp", str(jar), "tlc2.TLC",
             "-workers", "auto", "-metadir", work, "-config", str(config),
             str(model)],
            capture_output=True, text=True, cwd=model.parent,
        )
    log = result.stdout + result.stderr
    if result.returncode == 0 and "No error has been found" in log:
        return "pass", log
    match = VIOLATION.search(log)
    if not match:
        return "error", log
    if match["temporal"]:
        properties = re.findall(r"^PROPERTY\s+(\w+)", config.read_text(), re.M)
        return (properties[0] if len(properties) == 1 else "temporal"), log
    return match["invariant"] or match["action"], log


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("cases", nargs="*", help="configuration names; default all")
    args = parser.parse_args()
    jar = Path(os.environ.get(
        "TLA_TOOLS_JAR", "~/.local/share/tlaplus/1.7.4/tla2tools.jar")).expanduser()
    if not jar.is_file():
        sys.exit("Set TLA_TOOLS_JAR to tla2tools.jar")
    java = find_java()
    unknown = set(args.cases) - CASES.keys()
    if unknown:
        sys.exit(f"Unknown cases: {', '.join(sorted(unknown))}")
    runs = [(ROOT / "WorkerCustody.tla", ROOT / f"{name}.cfg", expected)
            for name, expected in CASES.items() if not args.cases or name in args.cases]
    failed = 0
    for model, config, expected in runs:
        outcome, log = run_tlc(java, jar, model, config)
        want = expected or "pass"
        ok = outcome == want
        failed += not ok
        print(f"{'ok  ' if ok else 'FAIL'} {model.stem}/{config.stem}: {outcome}"
              + ("" if ok else f" (expected {want})"))
        if not ok:
            print(log, file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
