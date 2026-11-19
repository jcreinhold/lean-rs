"""Provision or qualify the reviewed macOS arm64 Lean 4.36 metadata fixture."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile

SDK = Path(__file__).resolve().parents[1]
FIXTURE_SDK = "945792112a180bd9e3a90194159e0c9c158cde2d"
PACKET = "30f9897773b105a06c2cf5f7db845a2e70669b7bfa45ea318947ea99772605e0"
WRITER = "be59b60cccc490ac29b802a2760686c983a3d0ca95dd14a1cbefb264a3f3ae92"
TESTS = ("builder_opens_worker_capability_and_validates_metadata",
         "metadata_mismatch_is_reported_by_bootstrap_check")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def check_file(path, approved):
    require(path == path.resolve(strict=True) and path.is_file(), f"expected a canonical file: {path}")
    require(digest(path) == approved, f"SHA-256 mismatch: {path}")


def space(path):
    require(shutil.disk_usage(path).free >= 25 * 2**30, f"less than 25 GiB free on {path}")


def environment(root, helper, target):
    # These are trusted build tools. In particular, no loader overrides or Cargo wrappers
    # cross into the readiness executable, writer, guardian or child.
    prefix = root / "release"
    cargo = Path.home() / ".cargo/bin"
    return dict(HOME=str(Path.home()), PATH=os.pathsep.join(map(str, [
        prefix / "bin", cargo, Path(sys.executable).parent, "/usr/bin", "/bin",
    ])), LEAN_SYSROOT=str(prefix), LEAN_NUM_THREADS="1", CARGO_BUILD_JOBS="1",
        CARGO_INCREMENTAL="0", CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE="true",
        LEAN_HOST_MCP_WRITER_EXECUTABLE=str(helper), D3_HELPER=str(helper),
        LEAN_RS_WORKER_OWNERS_DIRECTORY=str(root / "owners"))


def provision(packet, helper, release):
    space(Path(tempfile.gettempdir()))
    root = Path(tempfile.mkdtemp(prefix="lean-t0-metadata-")).resolve(strict=True)
    print(f"Private deployment: {root}", flush=True)
    with tarfile.open(fileobj=io.BytesIO(packet)) as archive:
        archive.extractall(root, members=[m for m in archive if m.name.startswith("./app/")], filter="data")
    tree = subprocess.check_output(["/usr/bin/git", "-C", SDK, "archive", FIXTURE_SDK])
    sdk = root / "sdk"
    sdk.mkdir()
    with tarfile.open(fileobj=io.BytesIO(tree)) as archive:
        archive.extractall(sdk, filter="data")
    subprocess.run(["/bin/cp", "-cR", release, root / "release"], check=True)
    (root / "owners").mkdir(mode=0o700)
    app = root / "app"
    project = app / "lean"
    project.mkdir()
    source = sdk / "fixtures/interop-shims"
    shutil.copytree(source / "LeanRsInteropConsumer", project / "LeanRsInteropConsumer")
    for name in ("LeanRsInteropConsumer.lean", "lean-toolchain"):
        shutil.copyfile(source / name, project / name)
    dependency = "../../crates/lean-rs/shims/lean-rs-interop-shims"
    lakefile = (source / "lakefile.lean").read_text()
    require(lakefile.count(dependency) == 1, "unexpected fixture Lake dependency")
    (project / "lakefile.lean").write_text(lakefile.replace(
        dependency, str(sdk / "crates/lean-rs-host/shims/lean-rs-interop-shims")))
    env = environment(root, helper, root / "target")
    cargo = ["cargo", "build", "--locked", "--offline", "-j1"]
    for cwd, cmd in [(project, ["lake", "update"]), (app, [*cargo, "--bin", "prepare"]),
                     (app, [root / "target/debug/prepare"]),
                     (app, [*cargo, "--features", "admitted", "--bin", "metadata-worker"])]:
        space(root)
        subprocess.run(cmd, cwd=cwd, env=env, check=True)
    space(root)
    for path in (root / "target/debug/metadata-worker", app / "deployment.json"):
        print(f"{digest(path)}  {path}")


def qualify(root, helper, packet, approved_worker, approved_deployment):
    space(root)
    require(root == root.resolve(strict=True) and root.stat().st_uid == os.getuid()
            and root.stat().st_mode & 0o077 == 0, "expected a private owned deployment directory")
    worker = root / "target/debug/metadata-worker"
    record = root / "app/deployment.json"
    check_file(worker, approved_worker)
    check_file(record, approved_deployment)
    # Compare the inputs that built and sealed this application with the selected
    # source packet. The independently approved binary hash binds the linked output.
    with tarfile.open(fileobj=io.BytesIO(packet)) as archive:
        for name in ("Cargo.toml", "Cargo.lock", "build.rs", "seal.py", "src/lib.rs",
                     "src/bin/prepare.rs", "src/bin/metadata-worker.rs"):
            with archive.extractfile(f"./app/{name}") as original:
                require((root / "app" / name).read_bytes() == original.read(), f"fixture source mismatch: {name}")
    d = json.loads(record.read_bytes())
    require(d["sysroot"] == str(root / "release") and d["project"] == str(root / "app/lean"),
            "deployment record belongs to a different application or release")
    # dyld runs before the app can check its own record. Recheck the private
    # release libraries sealed by the producer, while the owner keeps this deployment stable.
    libdir = root / "release/lib/lean"
    sealed = {Path(name): pin for name, pin in d["files"].items()
              if Path(name).parent == libdir and name.endswith(".dylib")}
    require(libdir / "libleanshared.dylib" in sealed and set(libdir.glob("*.dylib")) == sealed.keys(),
            "selected runtime library inventory changed")
    for image, (device, inode, sha256) in sealed.items():
        stat = image.stat()
        require((stat.st_dev, stat.st_ino) == (device, inode), f"runtime file identity changed: {image}")
        check_file(image, sha256)
    manifest = Path(d["consumer_manifest"])
    require(manifest.is_relative_to(root / "app/lean") and manifest == manifest.resolve(strict=True)
            and manifest.read_bytes() == d["consumer_json"].encode(), "consumer manifest differs from review")
    owners = root / "owners"
    require(owners == owners.resolve(strict=True) and owners.is_dir(), "missing deployment ownership registry")
    env = environment(root, helper, root / "selector-target")
    # Only the independently approved worker can be executed, even for a read-only probe.
    ready = subprocess.check_output([worker, "--lean-rs-ready-host-artifacts"], cwd=root, env=env)
    expected = dict(protocol=1, shim_manifest_path=d["shim_manifest"], shim_manifest_json=d["shim_json"],
                    artifact_roots=d["shim_roots"], shim_search_paths=d["shim_paths"])
    require(json.loads(ready) == expected, "readiness descriptor differs from the reviewed deployment")
    env["LEAN_RS_TEST_QUALIFIED_METADATA"] = json.dumps(dict(
        worker=str(worker), worker_sha256=approved_worker, helper=str(helper), helper_sha256=WRITER,
        owners=str(owners), sysroot=d["sysroot"], manifest=str(manifest), prepared=expected))
    env["CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER"] = "/usr/bin/env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH"
    space(root)
    for name in TESTS:
        subprocess.run(["cargo", "nextest", "run", "--locked", "--offline", "-j1", "--no-tests", "fail",
                        "-p", "lean-rs-worker-child", "--test", "capability_builder", "-E", f"test(={name})"],
                       cwd=SDK, env=env, check=True)
    space(root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--packet", type=Path, required=True)
    parser.add_argument("--writer", type=Path, required=True)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("prepare", help="build and seal a new deployment, then print candidate hashes")
    prepare.add_argument("--lean-release", type=Path, required=True)
    run = commands.add_parser("run", help="execute the two original selectors against a reviewed deployment")
    run.add_argument("deployment", type=Path)
    run.add_argument("--worker-sha256", required=True)
    run.add_argument("--deployment-sha256", required=True)
    args = parser.parse_args()
    try:
        require((sys.platform, platform.machine()) == ("darwin", "arm64"), "requires macOS arm64")
        packet = args.packet.read_bytes()
        require(hashlib.sha256(packet).hexdigest() == PACKET, "source packet SHA-256 mismatch")
        check_file(args.writer, WRITER)
        if args.command == "prepare":
            release = args.lean_release.resolve(strict=True)
            check_file(release / "bin/lean", "a4c5d3dfc1672faf014e59bd17724035259ff0d22a4ffbdf5a75e142f564817e")
            check_file(release / "bin/lake", "a0e516de0ba03a26cb90c80f06666022ef9de3f0943f8b26e9b902d229b43ebe")
            check_file(release / "lib/lean/libleanshared.dylib", "9fa4e429820684b59ff37740ca77744a2cd6de70e6e963ef0904760c6921aed5")
            provision(packet, args.writer, release)
        else:
            qualify(args.deployment, args.writer, packet, args.worker_sha256, args.deployment_sha256)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, tarfile.TarError) as error:
        parser.exit(1, f"metadata qualification failed: {error}\n")


if __name__ == "__main__":
    main()
