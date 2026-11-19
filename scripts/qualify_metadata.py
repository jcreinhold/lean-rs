"""Provision or qualify the reviewed ordinary Lean 4.36 worker fixture."""

import argparse
import fcntl
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
from contextlib import contextmanager
from pathlib import Path

SDK = Path(__file__).resolve().parents[1]
FIXTURE_SDK = "945792112a180bd9e3a90194159e0c9c158cde2d"
APP_TREE = "e50fe473928dede19987ab655000f80a8c5a74cd"
TYPED_APP_TREE = "9a37c723ce30cce1f8a63c5206a377b5e7632d7b"
# Metadata deployments sealed before the typed policy used this revision of the same app.
ORIGINAL_APP_TREE = "ee1bf0cecda10560c7b58b33ea679188f87fea9c"
TYPED_SDK = "9914894988ec3bfd123f1d10b407c066eb7c6888"
LAKE_SHA256 = "a0e516de0ba03a26cb90c80f06666022ef9de3f0943f8b26e9b902d229b43ebe"
TESTS = (
    "builder_opens_worker_capability_and_validates_metadata",
    "metadata_mismatch_is_reported_by_bootstrap_check",
)
APP = SDK / "fixtures/metadata-qualification/app"
TARGET = SDK / "target"
STATE = TARGET / "metadata-qualification/ordinary"
STATE_LOCK = TARGET / "metadata-qualification/.ordinary.lock"
SOCKET_ROOT = TARGET / "ws"
BROKEN_IMPORT_TEST = "a_host_handle_built_with_a_broken_import_still_fails_at_open"
ORDINARY_TYPED_TESTS = (
    "describe_returns_row_for_known_declaration",
    "describe_returns_none_for_unknown_declaration",
    "search_declarations_returns_bounded_metadata_without_types",
    "search_declarations_supports_structural_filters_and_deterministic_order",
    "search_declarations_caps_broad_queries_and_reports_pruning",
    "declaration_type_truncates_single_rendered_type",
    "declaration_type_zero_cap_returns_empty_truncated_type",
    "inspect_declaration_returns_known_theorem",
    "inspect_declaration_returns_known_definition",
    "inspect_declaration_returns_not_found_for_unknown_name",
    "inspect_declaration_bounds_docstring_and_skips_proof_search_by_default",
    "inspect_declaration_reports_proof_search_when_requested",
    "inspect_declaration_truncates_large_type",
    "inspect_declaration_default_stays_under_total_budget",
    "search_result_name_can_be_inspected",
    "list_declarations_strings_streams_full_env_without_frame_cap",
    "describe_bulk_preserves_input_length_with_missing_slots",
    "oversized_terminal_response_is_request_error_not_child_death",
    "inspect_declaration_pretty_and_raw_rendering_differ",
)
ORDINARY_SESSION_REUSE_TESTS = (
    "reopening_an_identical_session_does_not_import_again",
    "max_imports_does_not_restart_on_an_identical_reopen",
    "a_full_pool_evicts_least_recently_used_and_re_imports",
    "a_pool_with_room_serves_the_return_without_importing",
    "pool_capacity_is_independent_of_the_import_restart_bound",
    "an_explicit_child_environment_outranks_the_configured_capacity",
)
ORDINARY_APP_TEST_COUNT = len(ORDINARY_TYPED_TESTS) + len(ORDINARY_SESSION_REUSE_TESTS)
MAX_FIXTURE_WORKER_BYTES = 256 * 1024**2
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

LINUX_X86_64_BROKEN_IMPORT_PINS = {
    "lean": "db7f559d05795e7af52e42bc966c90240a39dcb9712161c54c97afa546e20c00",
    "lake": "42678ccbc2605e2f892540dc7236e009fdd1224e823d104dfd72725d2f1a52d2",
    "runtime": "e2c403b411497bbc325d0a26bf57ba0132271750bf6d84e32cd61a9db4777252",
    "runtime_name": "libleanshared.so",
    **LINUX_X86_64_NEXTEST_RUNNER,
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


@contextmanager
def ordinary_state_lock():
    directory = STATE_LOCK.parent
    directory.mkdir(parents=True, exist_ok=True)
    require(
        directory == directory.resolve(strict=True), "ordinary state parent is aliased"
    )
    fd = os.open(STATE_LOCK, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    lock = os.fdopen(fd, "r+b")
    try:
        stat = os.fstat(lock.fileno())
        require(
            stat.st_uid == os.getuid() and stat.st_mode & 0o077 == 0,
            "ordinary state lock is not private to its owner",
        )
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError(
                "another ordinary metadata action owns the stable state lock"
            ) from error
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


def verified_platform(system=None, machine=None, *, broken_import=False):
    system = platform.system() if system is None else system
    machine = platform.machine() if machine is None else machine
    if (system, machine) == ("Darwin", "arm64"):
        return DARWIN_ARM64_PINS
    if (system, machine) == ("Linux", "x86_64") and broken_import:
        return LINUX_X86_64_BROKEN_IMPORT_PINS
    if system == "Linux":
        raise ValueError(
            "Linux ordinary dispatch awaits verified Lean release, helper and worker artifact pins"
        )
    raise ValueError(f"unsupported metadata qualification platform: {system} {machine}")


def refuse_inherited_loader_overrides(system=None, environ=None):
    system = platform.system() if system is None else system
    environ = os.environ if environ is None else environ
    if system == "Linux":
        overrides = sorted(
            name
            for name in environ
            if name == "GLIBC_TUNABLES"
            or name.startswith(("DYLD_", "__XPC_DYLD_", "LD_"))
        )
        require(
            not overrides, f"inherited Linux loader override: {', '.join(overrides)}"
        )


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
    require(
        release.parent == toolchains.resolve(strict=True),
        "repository Lean toolchain did not resolve inside the installed Elan toolchains directory",
    )
    inherited = os.environ.get("LEAN_SYSROOT")
    if inherited is not None:
        require(
            Path(inherited).expanduser().resolve(strict=True) == release,
            "inherited LEAN_SYSROOT differs from the repository Lean toolchain pin",
        )
    check_file(release / "bin/lean", profile["lean"])
    check_file(release / "bin/lake", profile["lake"])
    check_file(release / "lib/lean" / profile["runtime_name"], profile["runtime"])
    return release


def check_tracked_fixture_sources():
    for path in [
        APP / "Cargo.toml",
        APP / "Cargo.lock",
        APP / "build.rs",
        APP / "seal.py",
        APP / "src/lib.rs",
        APP / "src/bin/control.rs",
        APP / "src/bin/prepare.rs",
        APP / "src/bin/metadata-worker.rs",
        SDK / "fixtures/metadata-qualification/lean/lakefile.lean",
        SDK / "fixtures/metadata-qualification/lean/lake-manifest.json",
        SDK / "fixtures/metadata-qualification/lean/lean-toolchain",
        SDK / "fixtures/metadata-qualification/lean/LeanRsInteropConsumer.lean",
        SDK
        / "fixtures/metadata-qualification/lean/LeanRsInteropConsumer/Callback.lean",
    ]:
        require(
            path == path.resolve(strict=True) and path.is_file(),
            f"expected canonical fixture source: {path}",
        )
        relative = path.relative_to(SDK)
        subprocess.run(
            [
                "/usr/bin/git",
                "--no-replace-objects",
                "-C",
                SDK,
                "ls-files",
                "--error-unmatch",
                "--",
                relative,
            ],
            check=True,
            stdout=subprocess.DEVNULL,
        )


def check_file(path, approved):
    require(
        path == path.resolve(strict=True) and path.is_file(),
        f"expected a canonical file: {path}",
    )
    require(digest(path) == approved, f"SHA-256 mismatch: {path}")


def fixture_sources(tree):
    git = ["/usr/bin/git", "--no-replace-objects", "-C", SDK]
    entries = subprocess.check_output([*git, "ls-tree", "-rz", tree])
    sources = {}
    for entry in entries.rstrip(b"\0").split(b"\0"):
        header, name = entry.split(b"\t", 1)
        mode, kind, oid = header.split()
        require(
            (mode, kind) == (b"100644", b"blob"),
            "expected regular fixture source files",
        )
        sources[Path(os.fsdecode(name))] = subprocess.check_output(
            [*git, "cat-file", "blob", oid]
        )
    require(Path("Cargo.toml") in sources, "missing fixture app in pinned Git tree")
    return sources


def reviewed_writer(path, approved):
    require(
        path == path.resolve(strict=True)
        and path.is_file()
        and os.access(path, os.X_OK),
        f"expected a canonical executable writer: {path}",
    )
    data = path.read_bytes()
    require(hashlib.sha256(data).hexdigest() == approved, f"SHA-256 mismatch: {path}")
    return data


def space(path):
    require(
        shutil.disk_usage(path).free >= 25 * 2**30, f"less than 25 GiB free on {path}"
    )


def build_environment(release, scratch):
    # These are trusted build tools. In particular, no loader overrides or Cargo wrappers
    # cross into the readiness executable, writer, guardian or child.
    prefix = release
    cargo = Path.home() / ".cargo/bin"
    return {
        "HOME": str(Path.home()),
        "PATH": os.pathsep.join(
            map(
                str,
                [
                    prefix / "bin",
                    cargo,
                    Path(sys.executable).parent,
                    "/usr/bin",
                    "/bin",
                ],
            )
        ),
        "LEAN_SYSROOT": str(prefix),
        "LEAN_NUM_THREADS": "1",
        "CARGO_BUILD_JOBS": "1",
        "CARGO_INCREMENTAL": "0",
        "CARGO_TARGET_DIR": str(TARGET),
        "CARGO_NET_OFFLINE": "true",
        "TMPDIR": str(scratch),
    }


def environment(state, helper, release, deployment=None):
    env = build_environment(release, TARGET / "tmp")
    env.update(
        LEAN_HOST_MCP_WRITER_EXECUTABLE=str(helper),
        D3_HELPER=str(helper),
        LEAN_RS_WORKER_OWNERS_DIRECTORY=str(state / "owners"),
    )
    if deployment is not None:
        env.update(
            LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(deployment),
            LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(deployment),
            LEAN_RS_METADATA_AUDIT_PATH=str(state / "app/native-events.log"),
        )
    return env


def provision(
    writer,
    writer_sha256,
    release,
    typed_session=False,
    disposable_clean=False,
    verification=False,
):
    require(
        not (typed_session or disposable_clean or verification),
        "typed, verification and disposable-clean provisioning is unavailable in the canonical ordinary runner",
    )
    space(TARGET)
    check_tracked_fixture_sources()
    require(not STATE.exists(), f"ordinary deployment state already exists: {STATE}")
    require(
        not SOCKET_ROOT.exists(), f"ordinary socket root already exists: {SOCKET_ROOT}"
    )
    STATE.mkdir(parents=True, mode=0o700)
    SOCKET_ROOT.mkdir(mode=0o700)
    (STATE / "owners").mkdir(mode=0o700)
    (STATE / "app").mkdir(mode=0o700)
    helper = writer.resolve(strict=True)
    require(
        digest(helper) == writer_sha256, "reviewed helper changed before provisioning"
    )
    env = environment(STATE, helper, release)
    cargo = ["cargo", "build", "--locked", "--offline", "-j1"]
    for cmd in (
        [*cargo, "--bin", "prepare"],
        [str(TARGET / "debug/prepare"), str(STATE)],
    ):
        space(TARGET)
        subprocess.run(cmd, cwd=APP, env=env, check=True)
    record = STATE / "deployment.json"
    env = environment(STATE, helper, release, record)
    space(TARGET)
    subprocess.run(
        [
            *cargo,
            "--features",
            "admitted",
            "--bin",
            "metadata-worker",
            "--bin",
            "control",
        ],
        cwd=APP,
        env=env,
        check=True,
    )
    space(TARGET)
    write_phase(STATE, "prepared")
    for path in (TARGET / "debug/metadata-worker", TARGET / "debug/control", record):
        print(f"{digest(path)}  {path}")


def check_clean_deployment(root, approved_deployment):
    require(
        root.name.startswith("lean-e0-clean-"),
        "requires a fresh disposable clean deployment",
    )
    permit = root / "clean-ready"
    require(
        permit == permit.resolve(strict=True)
        and permit.read_text() == approved_deployment,
        "clean permit differs from the reviewed deployment",
    )
    require(
        not (root / "clean-started").exists()
        and not (root / "app/native-events.log").exists()
        and all(
            entry.name == ".admission.lock" for entry in (root / "owners").iterdir()
        ),
        "clean deployment has already been used",
    )
    project = root / "app/lean"
    build = project / ".lake/build"
    require(
        project == project.resolve(strict=True)
        and not project.is_relative_to(SDK)
        and build == build.resolve(strict=True)
        and build.is_dir(),
        "expected private, canonical Lake build directory",
    )
    # The selector rechecks these inputs under its writer before re-elaborating the package.
    config = {}
    for name in ("lakefile.lean", "lake-manifest.json", "lean-toolchain"):
        original = subprocess.check_output(
            [
                "/usr/bin/git",
                "--no-replace-objects",
                "-C",
                SDK,
                "show",
                f"{TYPED_SDK}:fixtures/lean/{name}",
            ]
        )
        config[name] = hashlib.sha256(original).hexdigest()
        check_file(project / name, config[name])
    check_file(root / "release/bin/lake", LAKE_SHA256)
    return {"deployment": approved_deployment, "config": config, "lake": LAKE_SHA256}


def scalar_fixture_inputs(project, deployment, helper, helper_sha256):
    image = project / ".lake/build/lib/liblean__rs__fixture_LeanRsFixture.dylib"
    require(
        str(project) == deployment["project"]
        and str(image) == deployment["consumer_image"]
        and str(image) == json.loads(deployment["consumer_json"])["primary_dylib"],
        "scalar test requires the sealed typed consumer image",
    )
    device, inode, sha256 = deployment["files"][str(image)]
    require(
        image == image.resolve(strict=True) and image.is_file(),
        "expected canonical scalar fixture image",
    )
    with image.open("rb") as source:
        stat = os.fstat(source.fileno())
        require(
            (stat.st_dev, stat.st_ino) == (device, inode),
            "scalar fixture image identity changed",
        )
        require(
            hashlib.file_digest(source, "sha256").hexdigest() == sha256,
            "scalar fixture image digest changed",
        )
    return {
        "LEAN_RS_TEST_SCALAR_FIXTURE": json.dumps([str(project), device, inode]),
        "LEAN_RS_PROFILE_FIXTURE_SHA256": sha256,
        "LEAN_RS_PROFILE_READER": str(helper),
        "LEAN_RS_PROFILE_READER_SHA256": helper_sha256,
    }


def check_installed_toolchain_pins(deployment, release):
    selected = [
        (Path(name), pin)
        for name, pin in deployment["files"].items()
        if Path(name).is_relative_to(release)
    ]
    require(selected, "deployment does not pin the selected installed Lean toolchain")
    for path, (device, inode, sha256) in selected:
        stat = path.stat()
        require(
            (stat.st_dev, stat.st_ino) == (device, inode),
            f"installed Lean toolchain file identity changed: {path}",
        )
        check_file(path, sha256)


def check_unavailable_typed_audit(events, before, verification):
    """Retained policy assertions for typed modes while this runner cannot provision them safely."""
    after = events.read_bytes()
    require(after.startswith(before), "native fixture event history changed")
    grants = [line.split(" ", 1) for line in after[len(before) :].decode().splitlines()]
    require(
        grants and len({pid for pid, _ in grants}) == 1,
        "expected a single fresh fixture worker",
    )
    names = [event for _, event in grants]
    required = [
        "verified-deployment",
        "authorize-host-load",
        "verified-global-interop-initializer",
        "authorize-LeanRsFixture-load",
        "authorize-Handles-SourceRanges-import",
    ]
    require(
        names[: len(required)] == required,
        f"unexpected fixture admission order: {names}",
    )
    if verification:
        expected = required + [
            "authorize-verify-proved",
            "authorize-verify-sorry",
            "authorize-verify-unavailable",
            "refused-verification-options",
            "authorize-verify-truncated",
        ]
        require(names == expected, f"unexpected verification grants: {names}")


def qualify(
    root,
    writer,
    writer_sha256,
    approved_worker,
    approved_deployment,
    typed_session=False,
    disposable_clean=False,
    verification=False,
    unknown_declaration=False,
    pool_validator_recovery=False,
    release=None,
    approved_control=None,
    profile=None,
):
    require(
        not (typed_session or disposable_clean or verification or unknown_declaration),
        "typed, verification and disposable-clean selectors are unavailable in the canonical ordinary runner",
    )
    profile = verified_platform() if profile is None else profile
    release = installed_lean_release(profile) if release is None else release
    space(TARGET)
    require(
        not pool_validator_recovery
        or (
            not typed_session
            and not disposable_clean
            and not verification
            and not unknown_declaration
        ),
        "pool validator recovery requires its named ordinary metadata route",
    )
    require(
        root == STATE and root == root.resolve(strict=True),
        "unexpected or aliased ordinary deployment state path",
    )
    require(
        root.stat().st_uid == os.getuid() and root.stat().st_mode & 0o777 == 0o700,
        "ordinary deployment state is not private to its owner",
    )
    require(
        SOCKET_ROOT == SOCKET_ROOT.resolve(strict=True)
        and SOCKET_ROOT.is_dir()
        and SOCKET_ROOT.stat().st_uid == os.geteuid()
        and SOCKET_ROOT.stat().st_mode & 0o777 == 0o700,
        "ordinary socket root is not canonical and private",
    )
    require(
        not any(SOCKET_ROOT.iterdir()),
        "ordinary socket root is not empty before dispatch",
    )
    check_tracked_fixture_sources()
    worker = TARGET / "debug/metadata-worker"
    control = TARGET / "debug/control"
    record = root / "deployment.json"
    check_file(worker, approved_worker)
    check_file(control, approved_control)
    check_file(record, approved_deployment)
    d = json.loads(record.read_bytes())
    project = SDK / "fixtures/metadata-qualification/lean"
    require(
        d.get("typed_session") is False and d.get("verification") is False,
        "ordinary deployment does not declare the ordinary fixture policy",
    )
    require(
        d.get("generated_output_key") == GENERATED_OUTPUT_KEY,
        "ordinary deployment selected a different generated-output slot",
    )
    require(
        d["sysroot"] == str(release) and d["project"] == str(project),
        "deployment record belongs to a different application or release",
    )
    # Elan does not participate in the project locks. Recheck the selected installed
    # toolchain before and after the smoke, and report any concurrent change as failure.
    check_installed_toolchain_pins(d, release)
    libdir = release / "lib/lean"
    suffix = Path(profile["runtime_name"]).suffix
    sealed = {
        Path(name): pin
        for name, pin in d["files"].items()
        if Path(name).parent == libdir and name.endswith(suffix)
    }
    require(
        libdir / profile["runtime_name"] in sealed
        and set(libdir.glob(f"*{suffix}")) == sealed.keys(),
        "selected runtime library inventory changed",
    )
    for image, (device, inode, sha256) in sealed.items():
        stat = image.stat()
        require(
            (stat.st_dev, stat.st_ino) == (device, inode),
            f"runtime file identity changed: {image}",
        )
        check_file(image, sha256)
    manifest = Path(d["consumer_manifest"])
    require(
        manifest.is_relative_to(project)
        and manifest == manifest.resolve(strict=True)
        and manifest.read_bytes() == d["consumer_json"].encode(),
        "consumer manifest differs from review",
    )
    owners = root / "owners"
    require(
        owners == owners.resolve(strict=True) and owners.is_dir(),
        "missing deployment ownership registry",
    )
    helper = writer.resolve(strict=True)
    require(
        digest(helper) == writer_sha256, "reviewed helper changed before qualification"
    )
    env = environment(root, helper, release, record)
    require(
        (root / "phase").read_text() == "prepared\n",
        "ordinary deployment was already dispatched",
    )
    write_phase(root, "dispatched")
    # Only the independently approved worker can be executed, even for a read-only probe.
    ready = subprocess.check_output(
        [
            worker,
            "--lean-rs-ready-host-artifacts-generated",
            SOCKET_ROOT,
            GENERATED_OUTPUT_KEY,
        ],
        cwd=APP,
        env=env,
    )
    expected = {
        "protocol": 2,
        "shim_manifest_path": d["shim_manifest"],
        "shim_manifest_json": d["shim_json"],
        "artifact_roots": d["shim_roots"],
        "shim_search_paths": d["shim_paths"],
    }
    require(
        json.loads(ready) == expected,
        "readiness descriptor differs from the reviewed deployment",
    )
    variable, target, tests = (
        "LEAN_RS_TEST_QUALIFIED_METADATA",
        "capability_builder",
        TESTS,
    )
    if pool_validator_recovery:
        tests = (
            "qualified_pool::failed_validator_releases_pool_slot_before_fresh_key",
        )
    selection = {"manifest": str(manifest)}
    env[variable] = json.dumps(
        dict(
            worker=str(worker),
            worker_sha256=approved_worker,
            helper=str(helper),
            helper_sha256=writer_sha256,
            owners=str(owners),
            sysroot=d["sysroot"],
            prepared=expected,
            **selection,
        )
    )
    select_nextest_runner(env, profile)
    space(TARGET)
    events = root / "app/native-events.log"
    before = events.read_bytes() if events.exists() else b""
    for name in tests:
        subprocess.run(
            [
                "cargo",
                "nextest",
                "run",
                "--locked",
                "--offline",
                "-j1",
                "--no-tests",
                "fail",
                "-p",
                "lean-rs-worker-child",
                "--test",
                target,
                *(
                    [
                        "--run-ignored",
                        "only",
                        "--retries",
                        "0",
                        "--success-output",
                        "immediate",
                    ]
                    if pool_validator_recovery
                    else []
                ),
                "-E",
                f"test(={name})",
            ],
            cwd=SDK,
            env=env,
            check=True,
        )
    after = events.read_bytes()
    require(after.startswith(before), "native fixture event history changed")
    grants = [line.split(" ", 1) for line in after[len(before) :].decode().splitlines()]
    blocks = []
    for pid, event in grants:
        if not blocks or blocks[-1][0] != pid:
            blocks.append((pid, []))
        blocks[-1][1].append(event)
    require(
        len(blocks) == 2 and blocks[0][0] != blocks[1][0],
        "expected exactly two distinct authorized fixture workers",
    )
    prefix = [
        "verified-deployment",
        "authorize-consumer-load",
        "authorize-host-load",
        "verified-global-interop-initializer",
    ]
    for _, names in blocks:
        require(
            names[: len(prefix)] == prefix
            and len(names) > len(prefix)
            and all(
                event == "authorize-Callback-import" for event in names[len(prefix) :]
            ),
            f"unexpected ordinary fixture admission order: {names}",
        )
    subprocess.run([control, "collect-closed"], cwd=APP, env=env, check=True)
    owner_entries = list(owners.iterdir())
    require(
        len(owner_entries) == 1
        and owner_entries[0].name == ".admission.lock"
        and owner_entries[0].is_file()
        and owner_entries[0] == owner_entries[0].resolve(strict=True)
        and owner_entries[0].stat().st_uid == os.getuid(),
        "worker ownership records remain after the selected checks",
    )
    check_installed_toolchain_pins(d, release)
    space(TARGET)
    require(
        not any(SOCKET_ROOT.iterdir()), "ordinary socket root retains a launch entry"
    )
    SOCKET_ROOT.rmdir()
    shutil.rmtree(root)


def fixture_worker_digest(worker):
    require(
        worker == worker.resolve(strict=True)
        and worker.is_file()
        and os.access(worker, os.X_OK)
        and worker.stat().st_size <= MAX_FIXTURE_WORKER_BYTES,
        "selected fixture worker is not a bounded executable",
    )
    with worker.open("rb") as stream:
        expected = os.fstat(stream.fileno()).st_size
        require(
            expected <= MAX_FIXTURE_WORKER_BYTES,
            "selected fixture worker grew before hashing",
        )
        sha256 = hashlib.sha256()
        remaining = expected
        while chunk := stream.read(min(64 * 1024, remaining + 1)):
            remaining -= len(chunk)
            require(remaining >= 0, "selected fixture worker grew while hashing")
            sha256.update(chunk)
        require(remaining == 0, "selected fixture worker shrank while hashing")
    return sha256.hexdigest()


def remove_owned_state(state, owner):
    closed = state.stat()
    require(
        state == state.resolve(strict=True)
        and (closed.st_dev, closed.st_ino) == (owner.st_dev, owner.st_ino),
        f"fixture directory changed: {state}",
    )
    shutil.rmtree(state)


def scoped_tests(binary, names):
    cases = " | ".join(f"test(={name})" for name in names)
    return f"(binary(={binary}) & ({cases}))"


def ordinary_app_tests():
    typed = scoped_tests("typed_session", ORDINARY_TYPED_TESTS)
    reuse = scoped_tests("session_reuse", ORDINARY_SESSION_REUSE_TESTS)
    return f"{typed} | {reuse}"


def check_typed_children_closed(events):
    with events.open("rb") as stream:
        data = stream.read(16 * 1024 + 1)
    require(
        0 < len(data) <= 16 * 1024 and data.endswith(b"\n"),
        "missing or incomplete typed worker audit",
    )
    admitted = set()
    workers = set()
    for line in data.splitlines():
        raw, event = line.split(b" ", 1)
        require(raw.isdigit() and int(raw) > 0, "invalid typed worker PID")
        pid = int(raw)
        workers.add(pid)
        if event == b"verified-deployment":
            admitted.add(pid)
    require(
        len(admitted) >= ORDINARY_APP_TEST_COUNT,
        "typed tests did not start the expected admitted workers",
    )
    for pid in workers:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue
        raise ValueError(f"typed worker PID {pid} remains; keeping fixture state")


def qualify_typed_group(parent, release, profile, run, env, cargo, descriptor):
    state = parent / "typed-session"
    state.mkdir(mode=0o700)
    owner = state.stat()
    scratch = state / "tmp"
    scratch.mkdir(mode=0o700)
    env.clear()
    env.update(
        build_environment(release, scratch), LEAN_RS_PREPARED_HOST_ARTIFACTS=descriptor
    )
    run([TARGET / "debug/prepare", state, "--typed-session"], APP)
    record = state / "deployment.json"
    require(
        record == record.resolve(strict=True) and record.is_file(),
        "missing exact typed fixture record",
    )
    audit_dir = state / "app"
    audit_dir.mkdir(mode=0o700)
    events = audit_dir / "native-events.log"
    env.update(
        LEAN_RS_METADATA_ORDINARY_TYPED="1",
        LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(record),
        LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(record),
        LEAN_RS_METADATA_AUDIT_PATH=str(events),
    )
    run([*cargo, "--features", "admitted", "--bin", "metadata-worker"], APP)
    worker = TARGET / "debug/metadata-worker"
    selection = json.dumps(
        {
            "worker": str(worker),
            "worker_sha256": fixture_worker_digest(worker),
            "fixture": str(SDK / "fixtures/lean"),
            "sysroot": str(release),
            "prepared": json.loads(descriptor),
        },
        separators=(",", ":"),
    )
    require(
        len(selection.encode()) <= 64 * 1024, "typed selection exceeds launch limit"
    )
    env["LEAN_RS_TEST_ORDINARY_TYPED_SESSION"] = selection
    select_nextest_runner(env, profile)
    run(
        [
            "cargo",
            "nextest",
            "run",
            "--locked",
            "--offline",
            "-j1",
            "--no-tests",
            "fail",
            "--retries",
            "0",
            "-p",
            "lean-rs-worker-child",
            "--test",
            "typed_session",
            "--test",
            "session_reuse",
            "-E",
            ordinary_app_tests(),
        ],
        SDK,
    )
    # A failed run or unconfirmed closure leaves this one state in place.
    check_typed_children_closed(events)
    remove_owned_state(state, owner)


def qualify_broken_import(
    release, profile, *, emit_host_selection=False, run_typed=False
):
    """Run one ordinary SDK worker test, without the downstream host helper."""
    require(TARGET == TARGET.resolve(strict=True), "aliased SDK target")
    parent = TARGET / "metadata-qualification"
    require(parent == parent.resolve(strict=True), "aliased fixture directory")
    timeout = (
        shutil.which("gtimeout")
        if platform.system() == "Darwin"
        else shutil.which("timeout")
    )
    require(timeout is not None, "GNU timeout is required for the broken-import check")
    if run_typed:
        require(emit_host_selection, "--run-typed requires --emit-host-selection")
        typed = parent / "typed-session"
        require(
            not os.path.lexists(typed), f"typed fixture state already exists: {typed}"
        )
    check_tracked_fixture_sources()
    space(TARGET)
    state = parent / "broken-import"
    state.mkdir(mode=0o700)  # Refuse a previous or uncertain invocation.
    owner = state.stat()
    scratch = state / "tmp"
    scratch.mkdir(mode=0o700)
    env = build_environment(release, scratch)

    def run(command, cwd):
        space(TARGET)
        subprocess.run(
            [timeout, "--signal=TERM", "--kill-after=15s", "8m", *command],
            cwd=cwd,
            env=env,
            check=True,
            **({"stdout": sys.stderr} if emit_host_selection else {}),
        )

    cargo = ["cargo", "build", "--locked", "--offline", "-j1"]
    run([*cargo, "--bin", "prepare"], APP)
    run([TARGET / "debug/prepare", state, "--broken-import"], APP)
    record = state / "deployment.json"
    require(
        record == record.resolve(strict=True) and record.is_file(),
        "missing exact fixture record",
    )
    env.update(
        LEAN_RS_METADATA_BROKEN_IMPORT="1",
        LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(record),
        LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(record),
    )
    run([*cargo, "--features", "admitted", "--bin", "metadata-worker"], APP)
    worker = TARGET / "debug/metadata-worker"
    env["LEAN_RS_TEST_BROKEN_IMPORT"] = json.dumps(
        {
            "project": str(state / "lean"),
            "worker": str(worker),
            "sysroot": str(release),
            "worker_sha256": fixture_worker_digest(worker),
        }
    )
    select_nextest_runner(env, profile)
    run(
        [
            "cargo",
            "nextest",
            "run",
            "--locked",
            "--offline",
            "-j1",
            "--no-tests",
            "fail",
            "--retries",
            "0",
            "-p",
            "lean-rs-worker-child",
            "--test",
            "capability_builder",
            "-E",
            f"test(={BROKEN_IMPORT_TEST})",
        ],
        SDK,
    )

    # The named test confirms both workers were reaped. On a failure or
    # timeout, keep this state and refuse further attempts pending closure.
    if emit_host_selection:
        with record.open("rb") as stream:
            data = stream.read(16 * 1024**2 + 1)
        require(len(data) <= 16 * 1024**2, "broken-import record exceeds fixture bound")
        require(
            hashlib.sha256(data).hexdigest()
            == env["LEAN_RS_METADATA_DEPLOYMENT_SHA256"],
            "broken-import selection changed after verification",
        )
        selected = json.loads(data)
        manifest = Path(selected["shim_manifest"])
        for name, image in (
            ("host_image", selected["host_image"]),
            ("interop_image", selected["interop_image"]),
        ):
            require(Path(image).is_relative_to(SDK), f"invalid {name} selection")
        image_pins = [
            [image, *selected["files"][image]]
            for image in (selected["host_image"], selected["interop_image"])
        ]
        host_selection = json.dumps(
            {
                "prefix": str(release),
                "manifest": str(manifest),
                "manifest_sha256": hashlib.sha256(
                    selected["shim_json"].encode()
                ).hexdigest(),
                "roots": selected["shim_roots"],
                "search_paths": selected["shim_paths"],
                "images": image_pins,
            },
            separators=(",", ":"),
        )
        typed_descriptor = json.dumps(
            {
                "protocol": 2,
                "shim_manifest_path": str(manifest),
                "shim_manifest_json": selected["shim_json"],
                "artifact_roots": selected["shim_roots"],
                "shim_search_paths": selected["shim_paths"],
            },
            separators=(",", ":"),
        )
        require(
            len(typed_descriptor.encode()) <= 64 * 1024,
            "prepared shim descriptor exceeds wire bound",
        )
    remove_owned_state(state, owner)
    if run_typed:
        qualify_typed_group(parent, release, profile, run, env, cargo, typed_descriptor)
    if emit_host_selection:
        print("LEAN_RS_TEST_ORDINARY_HOST_SHIMS=" + host_selection)
        print("LEAN_RS_PREPARED_HOST_ARTIFACTS=" + typed_descriptor)
        if run_typed:
            typed = ordinary_app_tests()
            broken = scoped_tests("capability_builder", (BROKEN_IMPORT_TEST,))
            print(
                "LEAN_RS_TEST_REMAINING_FILTER="
                f"not (package(=lean-rs-worker-child) & ({broken} | {typed}))"
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--writer", type=Path)
    parser.add_argument(
        "--writer-sha256", help="independently reviewed digest of this local helper"
    )
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument(
        "--disposable-clean",
        action="store_true",
        help="prepare or consume a one-use typed deployment to check guarded Lake clean",
    )
    modes.add_argument(
        "--typed-session",
        action="store_true",
        help="qualify the fixed LeanRsFixture Nat.add and scalar Boolean selectors",
    )
    modes.add_argument(
        "--verify-declaration",
        action="store_true",
        help="qualify four reviewed single-declaration requests on the typed fixture",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser(
        "prepare", help="build and seal a new deployment, then print candidate hashes"
    )
    broken = commands.add_parser(
        "broken-import",
        help="run the private SDK broken-import fixture without a writer helper",
    )
    broken.add_argument(
        "--emit-host-selection",
        action="store_true",
        help="emit checked environment assignments for these prepared shims",
    )
    broken.add_argument(
        "--run-typed", action="store_true", help="also run the fixed typed SDK tests"
    )
    run = commands.add_parser(
        "run", help="execute the selected tests against a reviewed deployment"
    )
    run.add_argument(
        "--unknown-declaration",
        action="store_true",
        help="with --typed-session, run only the unknown-declaration lookup",
    )
    run.add_argument(
        "--pool-validator-recovery",
        action="store_true",
        help="with the ordinary metadata fixture, run only the two-launch pool validator recovery check",
    )
    run.add_argument("deployment", type=Path)
    run.add_argument("--worker-sha256", required=True)
    run.add_argument("--control-sha256", required=True)
    run.add_argument("--deployment-sha256", required=True)
    args = parser.parse_args()
    if args.command == "broken-import":
        if args.writer is not None or args.writer_sha256 is not None:
            parser.error("broken-import does not use a writer helper")
        if args.run_typed and not args.emit_host_selection:
            parser.error("--run-typed requires --emit-host-selection")
    elif args.writer is None or args.writer_sha256 is None:
        parser.error("prepare and run require --writer and --writer-sha256")
    if args.command == "run" and args.unknown_declaration and not args.typed_session:
        parser.error("--unknown-declaration requires --typed-session")
    if args.command == "run" and args.pool_validator_recovery and args.typed_session:
        parser.error("--pool-validator-recovery requires the ordinary metadata fixture")
    if (
        args.command == "run"
        and args.pool_validator_recovery
        and args.unknown_declaration
    ):
        parser.error("--pool-validator-recovery conflicts with --unknown-declaration")
    if (
        args.typed_session
        or args.disposable_clean
        or args.verify_declaration
        or (args.command == "run" and args.unknown_declaration)
    ):
        parser.error(
            "typed, verification and disposable-clean selectors are temporarily unavailable; ordinary metadata only"
        )
    try:
        refuse_inherited_loader_overrides()
        profile = verified_platform(broken_import=args.command == "broken-import")
        release = installed_lean_release(profile)
        if args.command == "broken-import":
            with ordinary_state_lock():
                if args.run_typed:
                    qualify_broken_import(
                        release, profile, emit_host_selection=True, run_typed=True
                    )
                elif args.emit_host_selection:
                    qualify_broken_import(release, profile, emit_host_selection=True)
                else:
                    qualify_broken_import(release, profile)
            return
        reviewed_writer(args.writer, args.writer_sha256)
        writer = args.writer.resolve(strict=True)
        typed = args.typed_session or args.disposable_clean or args.verify_declaration
        with ordinary_state_lock():
            if args.command == "prepare":
                provision(
                    writer,
                    args.writer_sha256,
                    release,
                    typed,
                    args.disposable_clean,
                    args.verify_declaration,
                )
            else:
                qualify(
                    args.deployment,
                    writer,
                    args.writer_sha256,
                    args.worker_sha256,
                    args.deployment_sha256,
                    typed,
                    args.disposable_clean,
                    args.verify_declaration,
                    unknown_declaration=args.unknown_declaration,
                    pool_validator_recovery=args.pool_validator_recovery,
                    release=release,
                    approved_control=args.control_sha256,
                    profile=profile,
                )
    except (
        OSError,
        RuntimeError,
        ValueError,
        KeyError,
        subprocess.CalledProcessError,
    ) as error:
        parser.exit(1, f"metadata qualification failed: {error}\n")


if __name__ == "__main__":
    main()
