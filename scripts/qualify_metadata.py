"""Provision or qualify the reviewed macOS arm64 Lean 4.36 worker fixture."""
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
APP_TREE = "e50fe473928dede19987ab655000f80a8c5a74cd"
TYPED_APP_TREE = "9a37c723ce30cce1f8a63c5206a377b5e7632d7b"
# Metadata deployments sealed before the typed policy used this revision of the same app.
ORIGINAL_APP_TREE = "ee1bf0cecda10560c7b58b33ea679188f87fea9c"
TYPED_SDK = "9914894988ec3bfd123f1d10b407c066eb7c6888"
LAKE_SHA256 = "a0e516de0ba03a26cb90c80f06666022ef9de3f0943f8b26e9b902d229b43ebe"
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


def fixture_sources(tree):
    git = ["/usr/bin/git", "--no-replace-objects", "-C", SDK]
    entries = subprocess.check_output([*git, "ls-tree", "-rz", tree])
    sources = {}
    for entry in entries.rstrip(b"\0").split(b"\0"):
        header, name = entry.split(b"\t", 1)
        mode, kind, oid = header.split()
        require((mode, kind) == (b"100644", b"blob"), "expected regular fixture source files")
        sources[Path(os.fsdecode(name))] = subprocess.check_output([*git, "cat-file", "blob", oid])
    require(Path("Cargo.toml") in sources, "missing fixture app in pinned Git tree")
    return sources


def reviewed_writer(path, approved):
    require(path == path.resolve(strict=True) and path.is_file() and os.access(path, os.X_OK),
            f"expected a canonical executable writer: {path}")
    data = path.read_bytes()
    require(hashlib.sha256(data).hexdigest() == approved, f"SHA-256 mismatch: {path}")
    return data


def private_writer(directory, data, approved):
    helper = directory / "lean-host-mcp"
    if not helper.exists():
        with tempfile.TemporaryDirectory(dir=directory) as scratch:
            staged = Path(scratch) / "writer"
            staged.write_bytes(data)
            staged.chmod(0o500)
            try:
                os.link(staged, helper)
            except FileExistsError:
                pass  # Another run published its copy first. Check it below.
    check_file(helper, approved)
    require(os.access(helper, os.X_OK), f"private writer is not executable: {helper}")
    return helper


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


def provision(writer, writer_sha256, release, typed_session=False, disposable_clean=False, verification=False):
    space(Path(tempfile.gettempdir()))
    prefix = "lean-e0-clean-" if disposable_clean else "lean-t0-metadata-"
    root = Path(tempfile.mkdtemp(prefix=prefix)).resolve(strict=True)
    print(f"Private deployment: {root}", flush=True)
    helper = private_writer(root, writer, writer_sha256)
    app = root / "app"
    for name, data in fixture_sources(APP_TREE if verification else TYPED_APP_TREE).items():
        path = app / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    tree = subprocess.check_output(["/usr/bin/git", "--no-replace-objects", "-C", SDK, "archive", TYPED_SDK if typed_session else FIXTURE_SDK])
    sdk = root / "sdk"
    sdk.mkdir()
    with tarfile.open(fileobj=io.BytesIO(tree)) as archive:
        archive.extractall(sdk, filter="data")
    subprocess.run(["/bin/cp", "-cR", release, root / "release"], check=True)
    (root / "owners").mkdir(mode=0o700)
    project = app / "lean"
    if typed_session:
        # The SDK archive supplies tracked sources only. Build and seal in their final location.
        shutil.copytree(sdk / "fixtures/lean", project)
        require(not (project / ".lake").exists(), "expected a cold LeanRsFixture project")
        preparation = []
    else:
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
        preparation = [(project, ["lake", "update"])]
    env = environment(root, helper, root / "target")
    cargo = ["cargo", "build", "--locked", "--offline", "-j1"]
    prepare = [root / "target/debug/prepare"]
    if typed_session:
        prepare.append("--verify-declaration" if verification else "--typed-session")
    for cwd, cmd in preparation + [(app, [*cargo, "--bin", "prepare"]), (app, prepare),
                     (app, [*cargo, "--features", "admitted", "--bin", "metadata-worker"])]:
        space(root)
        subprocess.run(cmd, cwd=cwd, env=env, check=True)
    space(root)
    if disposable_clean:
        # A clean run consumes this newly provisioned deployment. Existing review roots have no permit.
        (root / "clean-ready").write_text(digest(app / "deployment.json"))
    for path in (root / "target/debug/metadata-worker", app / "deployment.json"):
        print(f"{digest(path)}  {path}")


def check_clean_deployment(root, approved_deployment):
    require(root.name.startswith("lean-e0-clean-"), "requires a fresh disposable clean deployment")
    permit = root / "clean-ready"
    require(permit == permit.resolve(strict=True) and permit.read_text() == approved_deployment,
            "clean permit differs from the reviewed deployment")
    require(not (root / "clean-started").exists() and not (root / "app/native-events.log").exists()
            and all(entry.name == ".admission.lock" for entry in (root / "owners").iterdir()),
            "clean deployment has already been used")
    project = root / "app/lean"
    build = project / ".lake/build"
    require(project == project.resolve(strict=True) and not project.is_relative_to(SDK)
            and build == build.resolve(strict=True) and build.is_dir(), "expected private, canonical Lake build directory")
    # The selector rechecks these inputs under its writer before re-elaborating the package.
    config = {}
    for name in ("lakefile.lean", "lake-manifest.json", "lean-toolchain"):
        original = subprocess.check_output(["/usr/bin/git", "--no-replace-objects", "-C", SDK,
                                            "show", f"{TYPED_SDK}:fixtures/lean/{name}"])
        config[name] = hashlib.sha256(original).hexdigest()
        check_file(project / name, config[name])
    check_file(root / "release/bin/lake", LAKE_SHA256)
    return dict(deployment=approved_deployment, config=config, lake=LAKE_SHA256)


def scalar_fixture_inputs(project, deployment, helper, helper_sha256):
    image = project / ".lake/build/lib/liblean__rs__fixture_LeanRsFixture.dylib"
    require(str(project) == deployment["project"] and str(image) == deployment["consumer_image"]
            and str(image) == json.loads(deployment["consumer_json"])["primary_dylib"],
            "scalar test requires the sealed typed consumer image")
    device, inode, sha256 = deployment["files"][str(image)]
    require(image == image.resolve(strict=True) and image.is_file(), "expected canonical scalar fixture image")
    with image.open("rb") as source:
        stat = os.fstat(source.fileno())
        require((stat.st_dev, stat.st_ino) == (device, inode), "scalar fixture image identity changed")
        require(hashlib.file_digest(source, "sha256").hexdigest() == sha256, "scalar fixture image digest changed")
    return dict(LEAN_RS_TEST_SCALAR_FIXTURE=json.dumps([str(project), device, inode]),
                LEAN_RS_PROFILE_FIXTURE_SHA256=sha256, LEAN_RS_PROFILE_READER=str(helper),
                LEAN_RS_PROFILE_READER_SHA256=helper_sha256)


def qualify(root, writer, writer_sha256, approved_worker, approved_deployment,
            typed_session=False, disposable_clean=False, verification=False):
    space(root)
    require(root == root.resolve(strict=True) and root.stat().st_uid == os.getuid()
            and root.stat().st_mode & 0o077 == 0, "expected a private owned deployment directory")
    worker = root / "target/debug/metadata-worker"
    record = root / "app/deployment.json"
    check_file(worker, approved_worker)
    check_file(record, approved_deployment)
    d = json.loads(record.read_bytes())
    require(d.get("typed_session", False) is typed_session, "deployment belongs to a different fixture policy")
    require(d.get("verification", False) is verification, "deployment belongs to a different verification policy")
    tree = TYPED_APP_TREE if "typed_session" in d else ORIGINAL_APP_TREE
    if verification:
        require(typed_session, "verification requires a typed deployment")
        tree = APP_TREE
    for name, expected_source in fixture_sources(tree).items():
        path = root / "app" / name
        require(path == path.resolve(strict=True) and path.read_bytes() == expected_source,
                f"fixture source differs from pinned Git tree: {name}")
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
    if disposable_clean:
        require(typed_session, "clean requires the typed fixture")
        clean_inputs = check_clean_deployment(root, approved_deployment)
    helper = private_writer(root, writer, writer_sha256)
    env = environment(root, helper, root / "selector-target")
    run_scalar = typed_session and not (disposable_clean or verification)
    if run_scalar:
        scalar_inputs = scalar_fixture_inputs(root / "app/lean", d, helper, writer_sha256)
    # Only the independently approved worker can be executed, even for a read-only probe.
    ready = subprocess.check_output([worker, "--lean-rs-ready-host-artifacts"], cwd=root, env=env)
    expected = dict(protocol=1, shim_manifest_path=d["shim_manifest"], shim_manifest_json=d["shim_json"],
                    artifact_roots=d["shim_roots"], shim_search_paths=d["shim_paths"])
    require(json.loads(ready) == expected, "readiness descriptor differs from the reviewed deployment")
    if typed_session:
        variable, target = "LEAN_RS_TEST_QUALIFIED_TYPED_SESSION", "typed_session"
        tests = ("describe_returns_row_for_known_declaration",)
        if disposable_clean:
            tests = ("disposable_clean::reader_blocks_clean_until_shutdown",)
        elif verification:
            variable = "LEAN_RS_TEST_QUALIFIED_VERIFICATION"
            tests = ("verify_declaration_accepts_closed_theorem_and_rejects_sorry",)
        selection = {"fixture": d["project"]}
    else:
        variable, target, tests = "LEAN_RS_TEST_QUALIFIED_METADATA", "capability_builder", TESTS
        selection = {"manifest": str(manifest)}
    if disposable_clean:
        env["LEAN_RS_TEST_DISPOSABLE_CLEAN"] = json.dumps(clean_inputs)
    env[variable] = json.dumps(dict(
        worker=str(worker), worker_sha256=approved_worker, helper=str(helper), helper_sha256=writer_sha256,
        owners=str(owners), sysroot=d["sysroot"], prepared=expected, **selection))
    env["CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER"] = "/usr/bin/env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH"
    space(root)
    if typed_session:
        events = root / "app/native-events.log"
        before = events.read_bytes() if events.exists() else b""
    for name in tests:
        subprocess.run(["cargo", "nextest", "run", "--locked", "--offline", "-j1", "--no-tests", "fail",
                        "-p", "lean-rs-worker-child", "--test", target,
                        *(["--run-ignored", "only", "--retries", "0", "--success-output", "immediate"] if disposable_clean else []),
                        "-E", f"test(={name})"],
                       cwd=SDK, env=env, check=True)
    if typed_session:
        after = events.read_bytes()
        require(after.startswith(before), "native fixture event history changed")
        grants = [line.split(" ", 1) for line in after[len(before):].decode().splitlines()]
        require(grants and len({pid for pid, _ in grants}) == 1, "expected a single fresh fixture worker")
        names = [event for _, event in grants]
        required = ["verified-deployment", "authorize-host-load", "verified-global-interop-initializer",
                    "authorize-LeanRsFixture-load", "authorize-Handles-SourceRanges-import"]
        require(names[:len(required)] == required, f"unexpected fixture admission order: {names}")
        if verification:
            expected_events = required + ["authorize-verify-proved", "authorize-verify-sorry",
                                          "authorize-verify-unavailable", "refused-verification-options",
                                          "authorize-verify-truncated"]
            require(names == expected_events, f"unexpected verification grants: {names}")
    if run_scalar:
        space(root)
        subprocess.run(["cargo", "nextest", "run", "--locked", "--offline", "-j1", "--no-tests", "fail",
                        "-p", "lean-rs", "--lib", "-E", "test(=abi::tests::bool_round_trip_via_fixture)"],
                       cwd=SDK, env={**env, **scalar_inputs}, check=True)
    space(root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--writer", type=Path, required=True)
    parser.add_argument("--writer-sha256", required=True, help="independently reviewed digest of this local helper")
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--disposable-clean", action="store_true",
                       help="prepare or consume a one-use typed deployment to check guarded Lake clean")
    modes.add_argument("--typed-session", action="store_true", help="qualify the fixed LeanRsFixture Nat.add and scalar Boolean selectors")
    modes.add_argument("--verify-declaration", action="store_true",
                       help="qualify four reviewed single-declaration requests on the typed fixture")
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("prepare", help="build and seal a new deployment, then print candidate hashes")
    prepare.add_argument("--lean-release", type=Path, required=True)
    run = commands.add_parser("run", help="execute the selected tests against a reviewed deployment")
    run.add_argument("deployment", type=Path)
    run.add_argument("--worker-sha256", required=True)
    run.add_argument("--deployment-sha256", required=True)
    args = parser.parse_args()
    try:
        require((sys.platform, platform.machine()) == ("darwin", "arm64"), "requires macOS arm64")
        writer = reviewed_writer(args.writer, args.writer_sha256)
        typed = args.typed_session or args.disposable_clean or args.verify_declaration
        if args.command == "prepare":
            release = args.lean_release.resolve(strict=True)
            check_file(release / "bin/lean", "a4c5d3dfc1672faf014e59bd17724035259ff0d22a4ffbdf5a75e142f564817e")
            check_file(release / "bin/lake", LAKE_SHA256)
            check_file(release / "lib/lean/libleanshared.dylib", "9fa4e429820684b59ff37740ca77744a2cd6de70e6e963ef0904760c6921aed5")
            provision(writer, args.writer_sha256, release, typed, args.disposable_clean, args.verify_declaration)
        else:
            qualify(args.deployment, writer, args.writer_sha256, args.worker_sha256, args.deployment_sha256,
                    typed, args.disposable_clean, args.verify_declaration)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, tarfile.TarError) as error:
        parser.exit(1, f"metadata qualification failed: {error}\n")


if __name__ == "__main__":
    main()
