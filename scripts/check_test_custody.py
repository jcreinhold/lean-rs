"""Check the selected platform helper on private fixture roots before native SDK tests."""

import argparse
import fcntl
import hashlib
import os
import shutil
import subprocess
import time
from pathlib import Path

CHECKOUT = Path(__file__).resolve().parents[1]
TARGET = CHECKOUT / "target"
WRITER = "LEAN_RS_WRITER_EXECUTABLE"


def qualify_lake_fixture(helper: Path, scratch: Path, env: dict[str, str]) -> None:
    """Build a cold SDK fixture through the standalone writer, and refuse one without it."""
    fixture = CHECKOUT / "fixtures/lean"
    tracked = subprocess.check_output(["git", "ls-files", "-z", "--", "."], cwd=fixture)
    inputs = [Path(os.fsdecode(name)) for name in tracked.split(b"\0") if name]
    env.update(LEAN_NUM_THREADS="1", CARGO_BUILD_JOBS="1")
    env.pop("OUT_DIR", None)  # Keep the SDK build cache inside each private fixture.
    env.pop(WRITER, None)
    command = [
        "cargo",
        "build",
        "--quiet",
        "--locked",
        "--offline",
        "-j1",
        "-p",
        "lean-rs-host",
        "--example",
        "lake_build_helper",
    ]
    subprocess.run(command, cwd=CHECKOUT, env=env, check=True)
    executable = TARGET / "debug/examples/lake_build_helper"
    approved = str(helper)
    cases = (
        ("sdk", {WRITER: approved}, ()),
        ("missing", {}, ("shared Lake writes require the configured writer helper",)),
    )
    for name, settings, rejection in cases:
        root = scratch / name
        for relative in inputs:
            dest = root / relative
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(fixture / relative, dest)
        siblings = set(scratch.iterdir())
        result = subprocess.run(
            [executable, root], cwd=CHECKOUT, env={**env, **settings}, capture_output=True, text=True
        )
        if not rejection:
            if result.returncode:
                raise ValueError(f"{name} build failed with exit {result.returncode}: {result.stderr}")
            verify_lake_outputs(root, result.stdout)
            continue
        if result.returncode == 0 or any(reason not in result.stderr for reason in rejection):
            raise ValueError(f"expected {name} refusal, got exit {result.returncode}: {result.stderr}")
        files = {path.relative_to(root) for path in root.rglob("*") if path.is_file()}
        if (
            set(scratch.iterdir()) != siblings
            or (root / ".lake").exists()
            or files != set(inputs)
            or any((root / path).read_bytes() != (fixture / path).read_bytes() for path in inputs)
        ):
            raise ValueError(f"{name} refusal changed the cold fixture or its lock domain: {root}")
        print(f"Refused {name} writer setting before creating fixture output or a project lock.")


def verify_lake_outputs(root: Path, output: str) -> None:
    libraries = [
        Path(line.removeprefix("dylib="))
        for line in output.splitlines()
        if line.startswith("dylib=")
    ]
    if len(libraries) != 1 or libraries[0].suffix not in (".so", ".dylib"):
        raise ValueError(f"expected one resolved fixture library, got {libraries}")
    libdir = root / ".lake/build/lib"
    modules = libdir / "lean/LeanRsFixture"
    for artifact in (
        libraries[0],
        modules / "Handles.olean",
        modules / "SourceRanges.olean",
    ):
        artifact = artifact.resolve(strict=True)
        if (
            not artifact.is_relative_to(libdir)
            or not artifact.is_file()
            or not artifact.stat().st_size
        ):
            raise ValueError(
                f"expected nonempty fixture output under {libdir}: {artifact}"
            )
        print(f"Published {root.name}/{artifact.relative_to(root)}")


def wait_for_writer_cleanup(scratch: Path, tmp: Path) -> None:
    # Successful build replies confirm the Lake command stopped. The standalone
    # writer still closes its scratch and lock descriptors after the caller exits.
    deadline = time.monotonic() + 5
    while True:
        try:
            for path in scratch.glob(".lean-rs-writer-*.lock"):
                with path.open("r+b") as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            if not any(tmp.iterdir()):
                return
        except BlockingIOError:
            pass
        if time.monotonic() >= deadline:
            raise ValueError(f"writer cleanup remains unconfirmed under {scratch}")
        time.sleep(0.02)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("helper", type=Path)
    parser.add_argument("approved_sha256")
    parser.add_argument(
        "--lake-fixture",
        action="store_true",
        help="qualify the writer setting for cold LeanRsFixture writes",
    )
    args = parser.parse_args()
    helper = args.helper
    if (
        helper.resolve(strict=True) != helper
        or not helper.is_file()
        or not os.access(helper, os.X_OK)
    ):
        parser.error("helper must be a canonical absolute executable")
    with helper.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != args.approved_sha256:
            parser.error("helper differs from the selected SHA-256")
    TARGET.mkdir(exist_ok=True)
    if TARGET.resolve(strict=True) != TARGET:
        parser.error("workspace target must be canonical")
    scratch = TARGET / "writer-custody-qualification"
    # An unfinished run may still own these project domains. Refuse reuse.
    scratch.mkdir(mode=0o700)
    owners, tmp = scratch / "owners", scratch / "tmp"
    owners.mkdir(mode=0o700)
    tmp.mkdir(mode=0o700)
    print(f"Private custody qualification: {scratch}", flush=True)
    env = dict(
        os.environ,
        CARGO_TARGET_DIR=str(TARGET),
        TMPDIR=str(tmp),
        LEAN_TOOLCHAIN_READER_TEST_HELPER=str(helper),
        LEAN_RS_WORKER_OWNERS_DIRECTORY=str(owners),
    )
    if args.lake_fixture:
        qualify_lake_fixture(helper, scratch, env)
    else:
        qualify_custody(env)
    wait_for_writer_cleanup(scratch, tmp)
    if any(entry.name != ".admission.lock" for entry in owners.iterdir()):
        raise ValueError(f"native custody remains under {scratch}")
    shutil.rmtree(scratch)


def qualify_custody(env: dict[str, str]) -> None:
    for case in (
        "real_fifo_proxy_does_not_leak_custody_to_unrelated_spawn",
        "real_writer_runs_command_in_selected_project",
    ):
        subprocess.run(
            [
                "cargo",
                "nextest",
                "run",
                "--locked",
                "--offline",
                "-p",
                "lean-toolchain",
                "--lib",
                "--run-ignored",
                "all",
                "--test-threads",
                "1",
                "-E",
                f"test(=shared_reader::tests::{case})",
            ],
            cwd=CHECKOUT,
            check=True,
            env=env,
        )


if __name__ == "__main__":
    main()
