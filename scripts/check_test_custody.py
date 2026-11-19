"""Check the selected platform helper on private fixture roots before native SDK tests."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def qualify_lake_fixture(helper: Path, owners: Path) -> None:
    """Exercise the same shared target as the typed-session fixture on two cold roots."""
    checkout = Path(__file__).resolve().parents[1]
    fixture = checkout / "fixtures/lean"
    tracked = subprocess.check_output(["git", "ls-files", "-z", "--", "."], cwd=fixture)
    inputs = [Path(os.fsdecode(name)) for name in tracked.split(b"\0") if name]
    scratch = Path(tempfile.mkdtemp(prefix="lean-test-lake-")).resolve(strict=True)
    print(f"Cold fixture copies: {scratch}", flush=True)
    approved, refused = scratch / "approved", scratch / "refused"
    for root in (approved, refused):
        for relative in inputs:
            dest = root / relative
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(fixture / relative, dest)
        if (root / ".lake").exists():
            raise ValueError(f"fixture already has Lake output: {root}")

    env = dict(os.environ, LEAN_RS_WORKER_OWNERS_DIRECTORY=str(owners),
               LEAN_NUM_THREADS="1", CARGO_BUILD_JOBS="1")
    env.pop("OUT_DIR", None)  # Keep the SDK build cache inside each private fixture.
    writer_var = "LEAN_HOST_MCP_WRITER_EXECUTABLE"
    env.pop(writer_var, None)
    command = ["cargo", "run", "--quiet", "--locked", "--offline", "-j1",
               "-p", "lean-rs-host", "--example", "lake_build_helper", "--"]
    result = subprocess.run([*command, approved], cwd=checkout, env={**env, writer_var: str(helper)},
                            stdout=subprocess.PIPE, text=True, check=True)
    libraries = [Path(line.removeprefix("dylib=")) for line in result.stdout.splitlines()
                 if line.startswith("dylib=")]
    if len(libraries) != 1 or libraries[0].suffix not in (".so", ".dylib"):
        raise ValueError(f"expected one resolved fixture library, got {libraries}")
    libdir = approved / ".lake/build/lib"
    modules = libdir / "lean/LeanRsFixture"
    for artifact in (libraries[0], modules / "Handles.olean", modules / "SourceRanges.olean"):
        artifact = artifact.resolve(strict=True)
        if not artifact.is_relative_to(libdir) or not artifact.is_file() or not artifact.stat().st_size:
            raise ValueError(f"expected nonempty fixture output under {libdir}: {artifact}")
        print(f"Published {artifact.relative_to(approved)}")

    result = subprocess.run([*command, refused], cwd=checkout, env=env, capture_output=True, text=True)
    missing = "could not acquire shared Lake writer: shared Lake writes require the configured writer helper"
    if result.returncode == 0 or "LakeOutputUnresolved" not in result.stderr or missing not in result.stderr:
        raise ValueError(f"expected missing-writer refusal, got exit {result.returncode}: {result.stderr}")
    files = {path.relative_to(refused) for path in refused.rglob("*") if path.is_file()}
    if (refused / ".lake").exists() or files != set(inputs) or any(
        (refused / name).read_bytes() != (fixture / name).read_bytes() for name in inputs
    ):
        raise ValueError(f"missing-writer attempt changed the cold fixture: {refused}")
    print("Refused without a configured writer; no fixture output published.")
    if any(entry.name != ".admission.lock" for entry in owners.iterdir()):
        raise ValueError(f"retained owner records in {owners}, fixture copies in {scratch}")
    shutil.rmtree(scratch)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("helper", type=Path)
    parser.add_argument("approved_sha256")
    parser.add_argument("--lake-fixture", action="store_true",
                        help="build the checkout LeanRsFixture and check refusal without a writer")
    args = parser.parse_args()
    helper = args.helper
    if helper.resolve(strict=True) != helper or not helper.is_file() or not os.access(helper, os.X_OK):
        parser.error("helper must be a canonical absolute executable")
    with helper.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != args.approved_sha256:
            parser.error("helper differs from the selected SHA-256")
    # Both tests allocate their own new project roots. Their owner namespace
    # therefore has no prior records or shared project consumers.
    owners = Path(tempfile.mkdtemp(prefix="lean-test-owners-")).resolve(strict=True)
    print(f"Private test owner registry: {owners}", flush=True)
    if args.lake_fixture:
        qualify_lake_fixture(helper, owners)
        shutil.rmtree(owners)
        return
    env = dict(os.environ, LEAN_TOOLCHAIN_READER_TEST_HELPER=str(helper),
               LEAN_RS_WORKER_OWNERS_DIRECTORY=str(owners))
    for case in ("real_fifo_proxy_does_not_leak_custody_to_unrelated_spawn",
                 "real_writer_runs_command_in_selected_project"):
        subprocess.run(["cargo", "nextest", "run", "--locked", "--offline",
                        "-p", "lean-toolchain", "--lib", "--run-ignored", "all",
                        "--test-threads", "1", "-E", f"test(=shared_reader::tests::{case})"],
                       check=True, env=env)
    shutil.rmtree(owners)



if __name__ == "__main__":
    main()
