"""Check the selected platform helper on private fixture roots before native SDK tests."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("helper", type=Path)
    parser.add_argument("approved_sha256")
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
