"""Provision or qualify the reviewed ordinary Lean 4.36 worker fixture."""
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys

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
APP = SDK / "fixtures/metadata-qualification/app"
TARGET = SDK / "target"
STATE = TARGET / "metadata-qualification/ordinary"
STATE_LOCK = TARGET / "metadata-qualification/.ordinary.lock"
SOCKET_ROOT = TARGET / "ws"
GENERATED_OUTPUT_KEY = "lean-4.36.0"
DARWIN_ARM64_PINS = {
    "lean": "a4c5d3dfc1672faf014e59bd17724035259ff0d22a4ffbdf5a75e142f564817e",
    "lake": LAKE_SHA256,
    "runtime": "9fa4e429820684b59ff37740ca77744a2cd6de70e6e963ef0904760c6921aed5",
    "runtime_name": "libleanshared.dylib",
    "nextest_runner_variable": "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
    "nextest_runner": "/usr/bin/env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH",
}
LINUX_X86_64_NEXTEST_RUNNER = {
    "nextest_runner_variable": "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
    "nextest_runner": "/usr/bin/env -u LD_LIBRARY_PATH",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


@contextmanager
def ordinary_state_lock():
    directory = STATE_LOCK.parent
    directory.mkdir(parents=True, exist_ok=True)
    require(directory == directory.resolve(strict=True), "ordinary state parent is aliased")
    fd = os.open(STATE_LOCK, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    lock = os.fdopen(fd, "r+b")
    try:
        stat = os.fstat(lock.fileno())
        require(stat.st_uid == os.getuid() and stat.st_mode & 0o077 == 0,
                "ordinary state lock is not private to its owner")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError("another ordinary metadata action owns the stable state lock") from error
        yield
    finally:
        lock.close()


def write_phase(state, phase):
    pending = state / ".phase.new"
    with pending.open("x") as stream:
        stream.write(phase + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    pending.replace(state / "phase")
    directory = os.open(state, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verified_platform(system=None, machine=None):
    system = platform.system() if system is None else system
    machine = platform.machine() if machine is None else machine
    if (system, machine) == ("Darwin", "arm64"):
        return DARWIN_ARM64_PINS
    if system == "Linux":
        raise ValueError("Linux ordinary dispatch awaits verified Lean release, helper and worker artifact pins")
    raise ValueError(f"unsupported metadata qualification platform: {system} {machine}")


def refuse_inherited_loader_overrides(system=None, environ=None):
    system = platform.system() if system is None else system
    environ = os.environ if environ is None else environ
    if system == "Linux":
        overrides = sorted(name for name in environ if name == "GLIBC_TUNABLES"
                           or name.startswith(("DYLD_", "__XPC_DYLD_", "LD_")))
        require(not overrides, f"inherited Linux loader override: {', '.join(overrides)}")


def select_nextest_runner(env, profile):
    """Select the reviewed target runner without changing the runner-entry environment."""
    variable = profile["nextest_runner_variable"]
    command = profile["nextest_runner"]
    require(variable not in env, f"duplicate target runner entry: {variable}")
    env[variable] = command


def installed_lean_release(profile=None):
    profile = verified_platform() if profile is None else profile
    pin = (SDK / "lean-toolchain").read_text().strip()
    require(pin and "\n" not in pin, "expected one repository Lean toolchain pin")
    directory = pin.replace("/", "--").replace(":", "---")
    elan_home = Path(os.environ.get("ELAN_HOME", Path.home() / ".elan")).expanduser()
    toolchains = elan_home / "toolchains"
    release = (toolchains / directory).resolve(strict=True)
    require(release.parent == toolchains.resolve(strict=True),
            "repository Lean toolchain did not resolve inside the installed Elan toolchains directory")
    inherited = os.environ.get("LEAN_SYSROOT")
    if inherited is not None:
        require(Path(inherited).expanduser().resolve(strict=True) == release,
                "inherited LEAN_SYSROOT differs from the repository Lean toolchain pin")
    check_file(release / "bin/lean", profile["lean"])
    check_file(release / "bin/lake", profile["lake"])
    check_file(release / "lib/lean" / profile["runtime_name"], profile["runtime"])
    return release


def check_tracked_fixture_sources():
    for path in [APP / "Cargo.toml", APP / "Cargo.lock", APP / "build.rs", APP / "seal.py", APP / "src/lib.rs",
                 APP / "src/bin/control.rs", APP / "src/bin/prepare.rs", APP / "src/bin/metadata-worker.rs",
                 SDK / "fixtures/metadata-qualification/lean/lakefile.lean",
                 SDK / "fixtures/metadata-qualification/lean/lake-manifest.json",
                 SDK / "fixtures/metadata-qualification/lean/lean-toolchain",
                 SDK / "fixtures/metadata-qualification/lean/LeanRsInteropConsumer.lean",
                 SDK / "fixtures/metadata-qualification/lean/LeanRsInteropConsumer/Callback.lean"]:
        require(path == path.resolve(strict=True) and path.is_file(), f"expected canonical fixture source: {path}")
        relative = path.relative_to(SDK)
        subprocess.run(["/usr/bin/git", "--no-replace-objects", "-C", SDK,
                        "ls-files", "--error-unmatch", "--", relative],
                       check=True, stdout=subprocess.DEVNULL)


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


def space(path):
    require(shutil.disk_usage(path).free >= 25 * 2**30, f"less than 25 GiB free on {path}")


def environment(state, helper, release, deployment=None):
    # These are trusted build tools. In particular, no loader overrides or Cargo wrappers
    # cross into the readiness executable, writer, guardian or child.
    prefix = release
    cargo = Path.home() / ".cargo/bin"
    env = dict(HOME=str(Path.home()), PATH=os.pathsep.join(map(str, [
        prefix / "bin", cargo, Path(sys.executable).parent, "/usr/bin", "/bin",
    ])), LEAN_SYSROOT=str(prefix), LEAN_NUM_THREADS="1", CARGO_BUILD_JOBS="1",
        CARGO_INCREMENTAL="0", CARGO_TARGET_DIR=str(TARGET), CARGO_NET_OFFLINE="true",
        TMPDIR=str(TARGET / "tmp"),
        LEAN_HOST_MCP_WRITER_EXECUTABLE=str(helper), D3_HELPER=str(helper),
        LEAN_RS_WORKER_OWNERS_DIRECTORY=str(state / "owners"))
    if deployment is not None:
        env.update(LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(deployment),
                   LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(deployment),
                   LEAN_RS_METADATA_AUDIT_PATH=str(state / "app/native-events.log"))
    return env


def provision(writer, writer_sha256, release, typed_session=False, disposable_clean=False, verification=False):
    require(not (typed_session or disposable_clean or verification),
            "typed, verification and disposable-clean provisioning is unavailable in the canonical ordinary runner")
    space(TARGET)
    check_tracked_fixture_sources()
    require(not STATE.exists(), f"ordinary deployment state already exists: {STATE}")
    require(not SOCKET_ROOT.exists(), f"ordinary socket root already exists: {SOCKET_ROOT}")
    STATE.mkdir(parents=True, mode=0o700)
    SOCKET_ROOT.mkdir(mode=0o700)
    (STATE / "owners").mkdir(mode=0o700)
    (STATE / "app").mkdir(mode=0o700)
    helper = writer.resolve(strict=True)
    require(digest(helper) == writer_sha256, "reviewed helper changed before provisioning")
    env = environment(STATE, helper, release)
    cargo = ["cargo", "build", "--locked", "--offline", "-j1"]
    for cmd in ([*cargo, "--bin", "prepare"], [str(TARGET / "debug/prepare"), str(STATE)]):
        space(TARGET)
        subprocess.run(cmd, cwd=APP, env=env, check=True)
    record = STATE / "deployment.json"
    env = environment(STATE, helper, release, record)
    space(TARGET)
    subprocess.run([*cargo, "--features", "admitted", "--bin", "metadata-worker", "--bin", "control"],
                   cwd=APP, env=env, check=True)
    space(TARGET)
    write_phase(STATE, "prepared")
    for path in (TARGET / "debug/metadata-worker", TARGET / "debug/control", record):
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


def check_installed_toolchain_pins(deployment, release):
    selected = [(Path(name), pin) for name, pin in deployment["files"].items()
                if Path(name).is_relative_to(release)]
    require(selected, "deployment does not pin the selected installed Lean toolchain")
    for path, (device, inode, sha256) in selected:
        stat = path.stat()
        require((stat.st_dev, stat.st_ino) == (device, inode),
                f"installed Lean toolchain file identity changed: {path}")
        check_file(path, sha256)


def check_unavailable_typed_audit(events, before, verification):
    """Retained policy assertions for typed modes while this runner cannot provision them safely."""
    after = events.read_bytes()
    require(after.startswith(before), "native fixture event history changed")
    grants = [line.split(" ", 1) for line in after[len(before):].decode().splitlines()]
    require(grants and len({pid for pid, _ in grants}) == 1, "expected a single fresh fixture worker")
    names = [event for _, event in grants]
    required = ["verified-deployment", "authorize-host-load", "verified-global-interop-initializer",
                "authorize-LeanRsFixture-load", "authorize-Handles-SourceRanges-import"]
    require(names[:len(required)] == required, f"unexpected fixture admission order: {names}")
    if verification:
        expected = required + ["authorize-verify-proved", "authorize-verify-sorry",
                               "authorize-verify-unavailable", "refused-verification-options",
                               "authorize-verify-truncated"]
        require(names == expected, f"unexpected verification grants: {names}")


def qualify(root, writer, writer_sha256, approved_worker, approved_deployment,
            typed_session=False, disposable_clean=False, verification=False, unknown_declaration=False,
            pool_validator_recovery=False, release=None, approved_control=None, profile=None):
    require(not (typed_session or disposable_clean or verification or unknown_declaration),
            "typed, verification and disposable-clean selectors are unavailable in the canonical ordinary runner")
    profile = verified_platform() if profile is None else profile
    release = installed_lean_release(profile) if release is None else release
    space(TARGET)
    require(not pool_validator_recovery or (
        not typed_session and not disposable_clean and not verification and not unknown_declaration),
        "pool validator recovery requires its named ordinary metadata route")
    require(root == STATE and root == root.resolve(strict=True), "unexpected or aliased ordinary deployment state path")
    require(root.stat().st_uid == os.getuid() and root.stat().st_mode & 0o777 == 0o700,
            "ordinary deployment state is not private to its owner")
    require(SOCKET_ROOT == SOCKET_ROOT.resolve(strict=True) and SOCKET_ROOT.is_dir()
            and SOCKET_ROOT.stat().st_uid == os.geteuid() and SOCKET_ROOT.stat().st_mode & 0o777 == 0o700,
            "ordinary socket root is not canonical and private")
    require(not any(SOCKET_ROOT.iterdir()), "ordinary socket root is not empty before dispatch")
    check_tracked_fixture_sources()
    worker = TARGET / "debug/metadata-worker"
    control = TARGET / "debug/control"
    record = root / "deployment.json"
    check_file(worker, approved_worker)
    check_file(control, approved_control)
    check_file(record, approved_deployment)
    d = json.loads(record.read_bytes())
    project = SDK / "fixtures/metadata-qualification/lean"
    require(d.get("typed_session") is False and d.get("verification") is False,
            "ordinary deployment does not declare the ordinary fixture policy")
    require(d.get("generated_output_key") == GENERATED_OUTPUT_KEY,
            "ordinary deployment selected a different generated-output slot")
    require(d["sysroot"] == str(release) and d["project"] == str(project),
            "deployment record belongs to a different application or release")
    # Elan does not participate in the project locks. Recheck the selected installed
    # toolchain before and after the smoke, and report any concurrent change as failure.
    check_installed_toolchain_pins(d, release)
    libdir = release / "lib/lean"
    suffix = Path(profile["runtime_name"]).suffix
    sealed = {Path(name): pin for name, pin in d["files"].items()
              if Path(name).parent == libdir and name.endswith(suffix)}
    require(libdir / profile["runtime_name"] in sealed and set(libdir.glob(f"*{suffix}")) == sealed.keys(),
            "selected runtime library inventory changed")
    for image, (device, inode, sha256) in sealed.items():
        stat = image.stat()
        require((stat.st_dev, stat.st_ino) == (device, inode), f"runtime file identity changed: {image}")
        check_file(image, sha256)
    manifest = Path(d["consumer_manifest"])
    require(manifest.is_relative_to(project) and manifest == manifest.resolve(strict=True)
            and manifest.read_bytes() == d["consumer_json"].encode(), "consumer manifest differs from review")
    owners = root / "owners"
    require(owners == owners.resolve(strict=True) and owners.is_dir(), "missing deployment ownership registry")
    helper = writer.resolve(strict=True)
    require(digest(helper) == writer_sha256, "reviewed helper changed before qualification")
    env = environment(root, helper, release, record)
    require((root / "phase").read_text() == "prepared\n", "ordinary deployment was already dispatched")
    write_phase(root, "dispatched")
    # Only the independently approved worker can be executed, even for a read-only probe.
    ready = subprocess.check_output(
        [worker, "--lean-rs-ready-host-artifacts-generated", SOCKET_ROOT, GENERATED_OUTPUT_KEY], cwd=APP, env=env)
    expected = dict(protocol=2, shim_manifest_path=d["shim_manifest"], shim_manifest_json=d["shim_json"],
                    artifact_roots=d["shim_roots"], shim_search_paths=d["shim_paths"])
    require(json.loads(ready) == expected, "readiness descriptor differs from the reviewed deployment")
    variable, target, tests = "LEAN_RS_TEST_QUALIFIED_METADATA", "capability_builder", TESTS
    if pool_validator_recovery:
        tests = ("qualified_pool::failed_validator_releases_pool_slot_before_fresh_key",)
    selection = {"manifest": str(manifest)}
    env[variable] = json.dumps(dict(
        worker=str(worker), worker_sha256=approved_worker, helper=str(helper), helper_sha256=writer_sha256,
        owners=str(owners), sysroot=d["sysroot"], prepared=expected, **selection))
    select_nextest_runner(env, profile)
    space(TARGET)
    events = root / "app/native-events.log"
    before = events.read_bytes() if events.exists() else b""
    for name in tests:
        subprocess.run(["cargo", "nextest", "run", "--locked", "--offline", "-j1", "--no-tests", "fail",
                        "-p", "lean-rs-worker-child", "--test", target,
                        *(["--run-ignored", "only", "--retries", "0", "--success-output", "immediate"]
                          if pool_validator_recovery else []),
                        "-E", f"test(={name})"],
                       cwd=SDK, env=env, check=True)
    after = events.read_bytes()
    require(after.startswith(before), "native fixture event history changed")
    grants = [line.split(" ", 1) for line in after[len(before):].decode().splitlines()]
    blocks = []
    for pid, event in grants:
        if not blocks or blocks[-1][0] != pid:
            blocks.append((pid, []))
        blocks[-1][1].append(event)
    require(len(blocks) == 2 and blocks[0][0] != blocks[1][0],
            "expected exactly two distinct authorized fixture workers")
    prefix = ["verified-deployment", "authorize-consumer-load", "authorize-host-load",
              "verified-global-interop-initializer"]
    for _, names in blocks:
        require(names[:len(prefix)] == prefix and len(names) > len(prefix)
                and all(event == "authorize-Callback-import" for event in names[len(prefix):]),
                f"unexpected ordinary fixture admission order: {names}")
    subprocess.run([control, "collect-closed"], cwd=APP, env=env, check=True)
    owner_entries = list(owners.iterdir())
    require(len(owner_entries) == 1 and owner_entries[0].name == ".admission.lock"
            and owner_entries[0].is_file() and owner_entries[0] == owner_entries[0].resolve(strict=True)
            and owner_entries[0].stat().st_uid == os.getuid(),
            "worker ownership records remain after the selected checks")
    check_installed_toolchain_pins(d, release)
    space(TARGET)
    require(not any(SOCKET_ROOT.iterdir()), "ordinary socket root retains a launch entry")
    SOCKET_ROOT.rmdir()
    shutil.rmtree(root)


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
    run = commands.add_parser("run", help="execute the selected tests against a reviewed deployment")
    run.add_argument("--unknown-declaration", action="store_true",
                     help="with --typed-session, run only the unknown-declaration lookup")
    run.add_argument("--pool-validator-recovery", action="store_true",
                     help="with the ordinary metadata fixture, run only the two-launch pool validator recovery check")
    run.add_argument("deployment", type=Path)
    run.add_argument("--worker-sha256", required=True)
    run.add_argument("--control-sha256", required=True)
    run.add_argument("--deployment-sha256", required=True)
    args = parser.parse_args()
    if args.command == "run" and args.unknown_declaration and not args.typed_session:
        parser.error("--unknown-declaration requires --typed-session")
    if args.command == "run" and args.pool_validator_recovery and args.typed_session:
        parser.error("--pool-validator-recovery requires the ordinary metadata fixture")
    if args.command == "run" and args.pool_validator_recovery and args.unknown_declaration:
        parser.error("--pool-validator-recovery conflicts with --unknown-declaration")
    if args.typed_session or args.disposable_clean or args.verify_declaration or \
            (args.command == "run" and args.unknown_declaration):
        parser.error("typed, verification and disposable-clean selectors are temporarily unavailable; ordinary metadata only")
    try:
        refuse_inherited_loader_overrides()
        profile = verified_platform()
        release = installed_lean_release(profile)
        reviewed_writer(args.writer, args.writer_sha256)
        writer = args.writer.resolve(strict=True)
        typed = args.typed_session or args.disposable_clean or args.verify_declaration
        with ordinary_state_lock():
            if args.command == "prepare":
                provision(writer, args.writer_sha256, release, typed, args.disposable_clean, args.verify_declaration)
            else:
                qualify(args.deployment, writer, args.writer_sha256, args.worker_sha256, args.deployment_sha256,
                        typed, args.disposable_clean, args.verify_declaration,
                        unknown_declaration=args.unknown_declaration,
                        pool_validator_recovery=args.pool_validator_recovery, release=release,
                        approved_control=args.control_sha256, profile=profile)
    except (OSError, RuntimeError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"metadata qualification failed: {error}\n")


if __name__ == "__main__":
    main()
