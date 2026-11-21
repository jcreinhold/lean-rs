"""Pre-execution controls for the ordinary metadata qualification."""

import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from scripts import qualify_metadata as qualification

SEAL_PATH = qualification.APP / "seal.py"
SEAL_SPEC = importlib.util.spec_from_file_location(
    "metadata_qualification_seal", SEAL_PATH
)
seal = importlib.util.module_from_spec(SEAL_SPEC)
SEAL_SPEC.loader.exec_module(seal)


REAL_SPACE = qualification.space


class MetadataQualificationTest(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(dir=qualification.TARGET)
        self.addCleanup(scratch.cleanup)
        self.target = Path(scratch.name).resolve()
        self.sdk = self.target / "sdk"
        self.sdk.mkdir()
        self.state = self.target / "metadata-qualification/ordinary"
        self.state.mkdir(parents=True, mode=0o700)
        self.socket_root = self.target / "ws"
        self.socket_root.mkdir(mode=0o700)
        (self.state / "phase").write_text("prepared\n")
        self.worker = self.target / "debug/metadata-worker"
        self.worker.parent.mkdir()
        self.worker.write_bytes(b"local worker")
        self.control = self.target / "debug/control"
        self.control.write_bytes(b"local control")
        self.record = self.state / "deployment.json"
        self.record.write_text("{}")
        self.helper = self.target / "writer"
        self.helper.write_bytes(b"local writer")
        self.helper.chmod(0o500)
        for name, value in (
            ("SDK", self.sdk),
            ("TARGET", self.target),
            ("STATE", self.state),
            ("SOCKET_ROOT", self.socket_root),
        ):
            self.enterContext(patch.object(qualification, name, value))
        self.enterContext(patch.object(qualification, "space"))
        self.enterContext(patch.object(qualification, "check_tracked_fixture_sources"))
        self.launch = self.enterContext(patch("subprocess.Popen"))

    def valid_record(self):
        project = self.sdk / "fixtures/metadata-qualification/lean"
        project.mkdir(parents=True)
        manifest = project / "consumer.json"
        manifest.write_text("{}")
        image = self.target / "lib/lean/libleanshared.dylib"
        image.parent.mkdir(parents=True)
        image.write_bytes(b"test native image")
        stat = image.stat()
        owners = self.state / "owners"
        owners.mkdir()
        (owners / ".admission.lock").touch(mode=0o600)
        (self.state / "app").mkdir()
        data = {
            "typed_session": False,
            "verification": False,
            "generated_output_key": qualification.GENERATED_OUTPUT_KEY,
            "sysroot": str(self.target),
            "project": str(project),
            "files": {
                str(image): [stat.st_dev, stat.st_ino, qualification.digest(image)]
            },
            "consumer_manifest": str(manifest),
            "consumer_json": "{}",
            "shim_manifest": "shim.json",
            "shim_json": "{}",
            "shim_roots": [],
            "shim_paths": [],
        }
        self.record.write_text(json.dumps(data))
        expected = {
            "protocol": 2,
            "shim_manifest_path": "shim.json",
            "shim_manifest_json": "{}",
            "artifact_roots": [],
            "shim_search_paths": [],
        }
        return owners, expected

    def qualify(self, worker_hash=None, record_hash=None, **options):
        qualification.qualify(
            self.state,
            self.helper,
            qualification.digest(self.helper),
            worker_hash or qualification.digest(self.worker),
            record_hash or qualification.digest(self.record),
            release=self.target,
            approved_control=qualification.digest(self.control),
            **options,
        )

    def test_wrong_worker_or_record_refuses_before_readiness(self):
        wrong = hashlib.sha256(b"substituted").hexdigest()
        for args, refused in [
            ({"worker_hash": wrong}, self.worker),
            ({"record_hash": wrong}, self.record),
        ]:
            with (
                self.subTest(refused=refused),
                self.assertRaisesRegex(ValueError, f"SHA-256 mismatch: {refused}"),
            ):
                self.qualify(**args)
        self.launch.assert_not_called()

    def test_wrong_control_refuses_before_readiness(self):
        with self.assertRaisesRegex(ValueError, f"SHA-256 mismatch: {self.control}"):
            qualification.qualify(
                self.state,
                self.helper,
                qualification.digest(self.helper),
                qualification.digest(self.worker),
                qualification.digest(self.record),
                release=self.target,
                approved_control="0" * 64,
            )
        self.launch.assert_not_called()

    def test_wrong_or_indirect_writer_refuses(self):
        approved = qualification.digest(self.helper)
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            qualification.reviewed_writer(self.helper, "0" * 64)
        alias = self.target / "writer-symlink"
        alias.symlink_to(self.helper)
        with self.assertRaisesRegex(
            ValueError, "expected a canonical executable writer"
        ):
            qualification.reviewed_writer(alias, approved)
        qualification.reviewed_writer(self.helper, approved)
        self.assertFalse((self.state / "lean-host-mcp").exists())
        self.launch.assert_not_called()

    def test_wrong_policy_refuses_before_readiness(self):
        self.record.write_text('{"typed_session": true, "verification": false}')
        with self.assertRaisesRegex(
            ValueError, "does not declare the ordinary fixture policy"
        ):
            self.qualify()
        self.launch.assert_not_called()

    def test_unsupported_routes_refuse_before_readiness(self):
        for mode in (
            "typed_session",
            "disposable_clean",
            "verification",
            "unknown_declaration",
        ):
            with (
                self.subTest(mode=mode),
                self.assertRaisesRegex(ValueError, "unavailable"),
            ):
                self.qualify(**{mode: True})
        self.launch.assert_not_called()

    def test_free_space_floor_refuses_before_any_launch(self):
        # Use the real floor check while controlling the reading.
        with (
            patch.object(qualification, "space", REAL_SPACE),
            patch.object(
                qualification.shutil,
                "disk_usage",
                return_value=SimpleNamespace(free=25 * 2**30 - 1),
            ),
            self.assertRaisesRegex(ValueError, "less than 25 GiB free"),
        ):
            self.qualify()
        self.launch.assert_not_called()

    def test_uncertain_readiness_is_not_replayed(self):
        self.valid_record()
        with patch.object(
            qualification.subprocess,
            "check_output",
            side_effect=subprocess.CalledProcessError(1, [str(self.worker)]),
        ) as readiness:
            with self.assertRaises(subprocess.CalledProcessError):
                self.qualify()
            self.assertEqual((self.state / "phase").read_text(), "dispatched\n")
            with self.assertRaisesRegex(
                (ValueError, RuntimeError), "already|started|consumed"
            ):
                self.qualify()
        readiness.assert_called_once()
        self.launch.assert_not_called()

    def test_success_collects_exact_registry_before_state_cleanup(self):
        owners, expected = self.valid_record()
        events = self.state / "app/native-events.log"
        prefix = [
            "verified-deployment",
            "authorize-consumer-load",
            "authorize-host-load",
            "verified-global-interop-initializer",
            "authorize-Callback-import",
        ]
        next_pid = iter((101, 202))

        def run(command, **_kwargs):
            if command[:3] == ["cargo", "nextest", "run"]:
                pid = next(next_pid)
                with events.open("a") as stream:
                    for event in prefix:
                        stream.write(f"{pid} {event}\n")
            elif command == [self.control, "collect-closed"]:
                self.assertEqual(
                    [path.name for path in owners.iterdir()], [".admission.lock"]
                )
            else:
                self.fail(f"unexpected command: {command}")
            return subprocess.CompletedProcess(command, 0)

        with (
            patch.object(
                qualification.subprocess,
                "check_output",
                return_value=json.dumps(expected).encode(),
            ) as readiness,
            patch.object(qualification.subprocess, "run", side_effect=run) as invoked,
        ):
            self.qualify()
        readiness.assert_called_once()
        command = readiness.call_args.args[0]
        self.assertEqual(
            command,
            [
                self.worker,
                "--lean-rs-ready-host-artifacts-generated",
                self.socket_root,
                qualification.GENERATED_OUTPUT_KEY,
            ],
        )
        self.assertNotIn(
            "LEAN_RS_WORKER_ARTIFACT_IPC_ROOT", readiness.call_args.kwargs["env"]
        )
        self.assertFalse(self.state.exists())
        self.assertFalse(self.socket_root.exists())
        self.assertEqual(
            invoked.call_args_list[-1].args[0], [self.control, "collect-closed"]
        )

    def test_failed_exact_collection_retains_dispatched_state(self):
        _owners, expected = self.valid_record()
        events = self.state / "app/native-events.log"
        prefix = [
            "verified-deployment",
            "authorize-consumer-load",
            "authorize-host-load",
            "verified-global-interop-initializer",
            "authorize-Callback-import",
        ]
        next_pid = iter((303, 404))

        def run(command, **_kwargs):
            if command[:3] == ["cargo", "nextest", "run"]:
                pid = next(next_pid)
                with events.open("a") as stream:
                    for event in prefix:
                        stream.write(f"{pid} {event}\n")
                return subprocess.CompletedProcess(command, 0)
            raise subprocess.CalledProcessError(1, command)

        with (
            patch.object(
                qualification.subprocess,
                "check_output",
                return_value=json.dumps(expected).encode(),
            ),
            patch.object(qualification.subprocess, "run", side_effect=run),
            self.assertRaises(subprocess.CalledProcessError),
        ):
            self.qualify()
        self.assertTrue(self.state.is_dir())
        self.assertEqual((self.state / "phase").read_text(), "dispatched\n")

    def test_reads_only_the_installed_repository_pin(self):
        sdk = self.target / "sdk"
        sdk.mkdir(exist_ok=True)
        (sdk / "lean-toolchain").write_text("leanprover/lean4:v4.36.0\n")
        elan = self.target / "elan"
        release = elan / "toolchains/selected-release"
        release.mkdir(parents=True)
        with (
            patch.object(qualification, "SDK", sdk),
            patch.dict(os.environ, {"ELAN_HOME": str(elan)}, clear=True),
            patch.object(qualification, "check_file") as check,
            patch.object(
                subprocess, "check_output", side_effect=[
                    str(release / "bin/lean"), str(release),
                    str(release / "bin/lean"), str(release / "bin/lean"),
                ],
            ) as resolve,
        ):
            self.assertEqual(qualification.installed_lean_release(), release)
            self.assertEqual(
                [call.args[0] for call in check.call_args_list],
                [
                    release / "bin/lean",
                    release / "bin/lake",
                    release / "lib/lean/libleanshared.dylib",
                ],
            )
            self.assertEqual(
                [call.args[0] for call in resolve.call_args_list],
                [
                    ["elan", "run", "leanprover/lean4:v4.36.0", "elan", "which", "lean"],
                    [sdk / "scripts/lean-release-prefix.sh", "4.36.0"],
                ],
            )
            with (
                patch.dict(os.environ, {"LEAN_SYSROOT": str(sdk)}),
                self.assertRaisesRegex(ValueError, "inherited LEAN_SYSROOT differs"),
            ):
                qualification.installed_lean_release()
            release.rmdir()
            with self.assertRaises(FileNotFoundError):
                qualification.installed_lean_release()
        self.launch.assert_not_called()

    def test_linux_refuses_without_verified_primary_artifact_pins(self):
        with self.assertRaisesRegex(
            ValueError, "awaits verified Lean release, helper and worker artifact pins"
        ):
            qualification.verified_platform("Linux", "x86_64")
        self.launch.assert_not_called()

    def test_build_environment_selects_separate_cargo_and_nextest_bins(self):
        rust = self.target / "rustup/bin"
        plugin = self.target / "install-action/bin"
        unrelated = self.target / "unrelated"
        for directory, name in ((rust, "cargo"), (plugin, "cargo-nextest"), (unrelated, "cargo-public-api")):
            directory.mkdir(parents=True)
            binary = directory / name
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
        homes = {"CARGO_HOME": str(self.target / "cargo-home"), "RUSTUP_HOME": str(self.target / "rustup-home")}
        inherited = {
            **homes,
            "PATH": os.pathsep.join(map(str, (unrelated, rust, plugin))),
            "LD_LIBRARY_PATH": "/unreviewed",
            "DYLD_LIBRARY_PATH": "/unreviewed",
            "RUSTC_WRAPPER": "/unreviewed/wrapper",
        }
        with patch.dict(os.environ, inherited, clear=True):
            env = qualification.build_environment(self.target / "lean", self.target)
        self.assertEqual(shutil.which("cargo", path=env["PATH"]), str(rust / "cargo"))
        self.assertEqual(shutil.which("cargo-nextest", path=env["PATH"]), str(plugin / "cargo-nextest"))
        self.assertNotIn(str(unrelated), env["PATH"].split(os.pathsep))
        self.assertEqual({key: env[key] for key in homes}, homes)
        self.assertTrue({"LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "RUSTC_WRAPPER"}.isdisjoint(env))

    def test_platform_profiles_select_only_the_reviewed_nextest_loader_cleanup(self):
        cases = [
            (
                qualification.DARWIN_ARM64_PINS,
                "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
                "/usr/bin/env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH",
            ),
            (
                qualification.LINUX_X86_64_NEXTEST_RUNNER,
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
                "/usr/bin/env -u LD_LIBRARY_PATH",
            ),
        ]
        for profile, variable, command in cases:
            with self.subTest(variable=variable):
                env = {"PATH": "/usr/bin", "LEAN_SYSROOT": "/selected"}
                qualification.select_nextest_runner(env, profile)
                self.assertEqual(
                    env,
                    {
                        "PATH": "/usr/bin",
                        "LEAN_SYSROOT": "/selected",
                        variable: command,
                    },
                )

    def test_platform_profile_refuses_inherited_target_runner_override(self):
        variable = qualification.LINUX_X86_64_NEXTEST_RUNNER["nextest_runner_variable"]
        env = {variable: "/unreviewed/runner"}
        with self.assertRaisesRegex(ValueError, "duplicate target runner entry"):
            qualification.select_nextest_runner(
                env, qualification.LINUX_X86_64_NEXTEST_RUNNER
            )
        self.assertEqual(env[variable], "/unreviewed/runner")
        self.launch.assert_not_called()

    def test_linux_cli_refuses_inherited_loader_override_before_platform_or_state(self):
        argv = [
            "qualify_metadata.py",
            "--writer",
            str(self.helper),
            "--writer-sha256",
            qualification.digest(self.helper),
            "prepare",
        ]
        with (
            patch.object(sys, "argv", argv),
            patch.object(qualification.platform, "system", return_value="Linux"),
            patch.dict(os.environ, {"LD_LIBRARY_PATH": "/unreviewed"}, clear=True),
            patch.object(qualification, "verified_platform") as platform_profile,
            patch.object(qualification, "ordinary_state_lock") as state_lock,
            self.assertRaises(SystemExit),
        ):
            qualification.main()
        platform_profile.assert_not_called()
        state_lock.assert_not_called()
        self.launch.assert_not_called()

    def test_linux_cli_refuses_before_state_lock_or_dispatch(self):
        argv = [
            "qualify_metadata.py",
            "--writer",
            str(self.helper),
            "--writer-sha256",
            qualification.digest(self.helper),
            "prepare",
        ]
        with (
            patch.object(sys, "argv", argv),
            patch.object(qualification.platform, "system", return_value="Linux"),
            patch.object(qualification.platform, "machine", return_value="x86_64"),
            patch.dict(os.environ, {}, clear=True),
            patch("sys.stderr", new_callable=io.StringIO) as error,
            patch.object(qualification, "ordinary_state_lock") as state_lock,
            self.assertRaises(SystemExit) as stopped,
        ):
            qualification.main()
        self.assertEqual(stopped.exception.code, 1)
        self.assertIn("awaits verified Lean release", error.getvalue())
        state_lock.assert_not_called()
        self.launch.assert_not_called()

    def mock_private_package(self):
        root = qualification.stream_package_root()
        manifest = root / "crates/lean-rs-host/shims/lean-rs-host-shims/.lake/lean-rs-generated/lean-4.36.0/lean-rs-build-cache/host.json"

        def prepare(_release):
            root.mkdir(mode=0o700)
            manifest.parent.mkdir(parents=True)
            manifest.write_text("{}")
            return root.stat(), manifest, []

        self.enterContext(patch.object(qualification, "prepare_stream_package", side_effect=prepare))
        return manifest

    def test_broken_import_runs_exact_case_with_no_helper_then_cleans(self):
        manifest = self.mock_private_package()
        state = self.state.with_name("broken-import")
        self.worker.chmod(0o500)
        seen = []

        def run(command, *, cwd, env, check):
            seen.append((command, cwd, dict(env), check))
            if command[-1] == "--broken-import":
                (state / "deployment.json").write_text("{}")

        with (
            patch.object(
                qualification.shutil, "which", return_value="/reviewed/gtimeout"
            ),
            patch.object(qualification.subprocess, "run", side_effect=run),
        ):
            qualification.qualify_broken_import(
                self.target, qualification.DARWIN_ARM64_PINS
            )

        self.assertEqual(len(seen), 3)
        self.assertEqual(json.loads(seen[0][2]["LEAN_RS_PREPARED_HOST_ARTIFACTS"])["shim_manifest_path"], str(manifest))
        self.assertFalse(qualification.stream_package_root().exists())
        for command, _, env, check in seen:
            self.assertEqual(
                command[:4],
                ["/reviewed/gtimeout", "--signal=TERM", "--kill-after=15s", "8m"],
            )
            self.assertNotIn("LEAN_HOST_MCP_WRITER_EXECUTABLE", env)
            self.assertNotIn("LEAN_RS_METADATA_AUDIT_PATH", env)
            self.assertTrue(check)
        command, cwd, env, _ = seen[-1]
        self.assertEqual(cwd, self.sdk)
        self.assertEqual(
            command[-2:], ["-E", f"test(={qualification.BROKEN_IMPORT_TEST})"]
        )
        self.assertEqual(command[command.index("--retries") + 1], "0")
        self.assertEqual(
            env["CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER"],
            qualification.DARWIN_ARM64_PINS["nextest_runner"],
        )
        self.assertEqual(
            json.loads(env["LEAN_RS_TEST_BROKEN_IMPORT"])["project"],
            str(state / "lean"),
        )
        self.assertFalse(state.exists())

    def test_broken_import_failure_retains_owned_state_and_refuses_replay(self):
        self.mock_private_package()
        state = self.state.with_name("broken-import")
        with (
            patch.object(
                qualification.shutil, "which", return_value="/reviewed/gtimeout"
            ),
            patch.object(
                qualification.subprocess,
                "run",
                side_effect=subprocess.CalledProcessError(124, "timeout"),
            ) as run,
        ):
            with self.assertRaises(subprocess.CalledProcessError):
                qualification.qualify_broken_import(
                    self.target, qualification.DARWIN_ARM64_PINS
                )
            self.assertTrue(state.is_dir())
            with self.assertRaises(FileExistsError):
                qualification.qualify_broken_import(
                    self.target, qualification.DARWIN_ARM64_PINS
                )
        run.assert_called_once()

    def run_typed_selection(
        self, *, live_pid=False, extra_json_worker=False, direct_tests=()
    ):
        manifest = self.mock_private_package()
        broken = self.state.with_name("broken-import")
        typed = self.state.with_name("typed-session")
        fixture = self.sdk / "fixtures/lean"
        fixture.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.worker.chmod(0o500)
        seen = []
        host, interop = (str(self.sdk / image) for image in ("host", "interop"))
        record = {
            "shim_manifest": str(manifest),
            "shim_json": "{}",
            "shim_roots": [str(self.sdk)],
            "shim_paths": [str(self.sdk)],
            "host_image": host,
            "interop_image": interop,
            "files": {host: [1, 2, "host-digest"], interop: [3, 4, "interop-digest"]},
        }

        def run(command, *, cwd, env, check, stdout):
            seen.append((command, dict(env)))
            if command[-1] == "--broken-import":
                (broken / "deployment.json").write_text(json.dumps(record))
            elif command[-1] == "--typed-session":
                self.assertEqual(env["LEAN_RS_TEST_TYPED_PROJECT"], str(fixture))
                native = (
                    "dylib" if qualification.platform.system() == "Darwin" else "so"
                )
                image = str(
                    fixture
                    / ".lake/build/lib"
                    / f"liblean__rs__fixture_LeanRsFixture.{native}"
                )
                typed_record = {
                    **record,
                    "project": str(fixture),
                    "consumer_image": image,
                    "files": {**record["files"], image: [5, 6, "fixture-digest"]},
                }
                (typed / "deployment.json").write_text(json.dumps(typed_record))
                (typed / "worker-json-capability.json").write_text("{}")
            elif (
                "nextest" in command
                and "typed_session" in command
                and "LEAN_RS_TEST_AUDIT_WORKER_START" not in env
            ):
                Path(env["LEAN_RS_METADATA_AUDIT_PATH"]).write_text(
                    "".join(
                        f"{pid} verified-deployment\n"
                        for pid in range(10, 10 + qualification.ORDINARY_APP_TEST_COUNT)
                    )
                )

            elif "nextest" in command and "LEAN_RS_TEST_AUDIT_WORKER_START" in env:
                audit = Path(env["LEAN_RS_METADATA_AUDIT_PATH"])
                self.assertFalse(
                    audit.exists(), "already checked audit was not cleared"
                )
                target, name, (counts, *_), event = next(
                    case
                    for case in qualification.ORDINARY_AUDITED_TESTS
                    if command[-1] == qualification.scoped_tests(case[0], (case[1],))
                )
                self.assertIn(target, command)
                started, grants = counts
                if extra_json_worker:
                    started += 1
                    grants += 1
                with audit.open("w") as stream:
                    for index in range(started):
                        pid = 51 + index
                        stream.write(f"{pid} worker-started\n")
                        if per_worker := qualification.ORDINARY_GRANTS_BY_WORKER.get(
                            name
                        ):
                            granted = (event,) * per_worker[index]
                        elif event in (
                            qualification.JSON_GRANT,
                            qualification.ADAPTER_IMPORT_GRANT,
                        ):
                            granted = (event,) if index < grants else ()
                        else:
                            granted = qualification.ORDINARY_MIXED_GRANTS.get(
                                name, (event,) * grants
                            )
                        if granted:
                            stream.write(f"{pid} verified-deployment\n")
                            for marker in granted:
                                stream.write(f"{pid} {marker.decode()}\n")

        output = io.StringIO()
        with (
            patch.object(
                qualification.shutil, "which", return_value="/reviewed/timeout"
            ),
            patch.object(qualification.subprocess, "run", side_effect=run),
            patch.object(
                qualification.os,
                "kill",
                side_effect=None if live_pid else ProcessLookupError,
            ) as probe,
            patch("sys.stdout", output),
            patch.object(
                qualification,
                "qualify_host_builder_cases",
                return_value=tuple(
                    case
                    for case in direct_tests
                    if case != qualification.DIRECT_OPTIONAL_WHNF_TEST
                ),
            ) as host_builder,
            patch.object(qualification, "qualify_scalar_entry") as scalar,
            patch.object(
                qualification,
                "qualify_optional_whnf",
                return_value=(qualification.DIRECT_OPTIONAL_WHNF_TEST,)
                if qualification.DIRECT_OPTIONAL_WHNF_TEST in direct_tests
                else (),
            ),
        ):
            if live_pid:
                with self.assertRaisesRegex(
                    ValueError, "remains; keeping fixture state"
                ):
                    qualification.qualify_broken_import(
                        self.target,
                        qualification.DARWIN_ARM64_PINS,
                        emit_host_selection=True,
                        run_typed=True,
                    )
                self.assertTrue(typed.exists())
                self.assertEqual(output.getvalue(), "")
                with self.assertRaisesRegex(
                    ValueError, "typed fixture state already exists"
                ):
                    qualification.qualify_broken_import(
                        self.target,
                        qualification.DARWIN_ARM64_PINS,
                        emit_host_selection=True,
                        run_typed=True,
                    )
            elif extra_json_worker:
                with self.assertRaisesRegex(
                    ValueError, "wrong started worker / authorize-consumer-load counts"
                ):
                    qualification.qualify_broken_import(
                        self.target,
                        qualification.DARWIN_ARM64_PINS,
                        emit_host_selection=True,
                        run_typed=True,
                    )
                self.assertTrue(typed.exists())
                self.assertEqual(output.getvalue(), "")
            else:
                qualification.qualify_broken_import(
                    self.target,
                    qualification.DARWIN_ARM64_PINS,
                    emit_host_selection=True,
                    run_typed=True,
                )
                self.assertFalse(typed.exists())
                self.assertEqual(
                    probe.call_count,
                    qualification.ORDINARY_APP_TEST_COUNT
                    + sum(
                        counts[0][0]
                        for _, _, counts, _ in qualification.ORDINARY_AUDITED_TESTS
                    ),
                )
        if live_pid or extra_json_worker:
            host_builder.assert_not_called()
            scalar.assert_not_called()
        else:
            host_builder.assert_called_once()
            scalar.assert_called_once()
            self.assertEqual(host_builder.call_args.args[-1], scalar.call_args.args[-1])
        self.assertFalse(broken.exists())
        self.assertEqual(
            len(seen),
            6
            if live_pid
            else 7
            if extra_json_worker
            else 6 + len(qualification.ORDINARY_AUDITED_TESTS),
        )
        return seen, output.getvalue()

    def test_typed_worker_rejects_shared_or_unselected_project_before_preparation(self):
        state = self.state.with_name("typed-session")
        state.mkdir(mode=0o700)
        shared = self.sdk / "fixtures/lean"
        shared.mkdir(mode=0o755, parents=True)
        wrong = state / "other"
        wrong.mkdir(mode=0o700)
        for fixture, error in (
            (shared, "typed fixture root is not private and job-owned"),
            (wrong, "unapproved typed fixture root"),
        ):
            with (
                self.subTest(fixture=fixture),
                self.assertRaisesRegex(ValueError, error),
            ):
                qualification.prepare_typed_worker(
                    state, fixture, self.target, None, {}, [], "{}"
                )
            self.assertFalse((state / "tmp").exists())

    def test_typed_group_reuses_prepared_shims_and_excludes_only_completed_tests(self):
        seen, output = self.run_typed_selection()
        (prepare, prepare_env), (build, build_env), (nextest, run_env) = seen[3:6]
        audited_cases = seen[6:]
        assignments = dict(line.split("=", 1) for line in output.splitlines())
        descriptor = assignments["LEAN_RS_PREPARED_HOST_ARTIFACTS"]
        self.assertEqual(prepare[-1], "--typed-session")
        self.assertEqual(prepare_env["LEAN_RS_PREPARED_HOST_ARTIFACTS"], descriptor)
        self.assertEqual(build[-1], "metadata-worker")
        self.assertEqual(build_env["LEAN_RS_METADATA_ORDINARY_TYPED"], "1")
        self.assertNotIn("LEAN_RS_METADATA_BROKEN_IMPORT", build_env)
        self.assertNotIn("LEAN_RS_TEST_BROKEN_IMPORT", run_env)
        self.assertEqual(nextest.count("-j1"), 1)
        self.assertNotIn("--test-threads", nextest)
        self.assertEqual(nextest[nextest.index("--retries") + 1], "0")
        self.assertEqual(
            json.loads(run_env["LEAN_RS_TEST_ORDINARY_TYPED_SESSION"])["prepared"],
            json.loads(descriptor),
        )
        selection = nextest[-1]
        self.assertIn("binary(=typed_session)", selection)
        self.assertIn("binary(=session_reuse)", selection)
        self.assertEqual(selection.count("test(="), 23)
        self.assertIn(
            "test(=process_module_query_batch_isolates_snapshots_by_session_imports)",
            selection,
        )
        self.assertNotIn(
            "test(=process_module_query_batch_changed_session_imports_rebuilds)",
            selection,
        )
        pool_cases = (
            "import_profile_partitions_pool_session_keys",
            "import_workspace_root_partitions_pool_session_keys",
            "explicit_cycle_invalidates_only_the_current_lease",
            "queue_wait_timeout_is_typed_when_pool_is_full",
            "per_worker_rss_policy_invalidates_old_lease_before_work",
            "idle_policy_invalidates_lease_and_fresh_lease_can_continue",
            "memory_budget_rejects_new_distinct_worker_when_known_rss_is_exhausted",
            "rss_snapshot_records_available_samples_on_supported_platforms",
        )
        meta_cases = (
            "infer_type_returns_rendered_type_for_known_term",
            "whnf_reduces_known_term",
            "is_def_eq_recognises_equal_terms",
        )
        adapter_cases = (
            ("session_adapter", "declaration_bulk_adapter_matches_in_process_session"),
            ("session_adapter", "progress_events_cross_worker_boundary_in_order"),
            (
                "session_adapter",
                "parent_can_cancel_long_worker_request_at_progress_boundary",
            ),
            ("supervisor", "max_import_policy_restarts_before_next_import"),
            ("session_adapter", "elaboration_and_kernel_adapter_match_status_shape"),
        )
        module_query_cases = (
            "process_module_query_handles_module_system_header_through_worker",
            "process_module_query_returns_diagnostics_through_worker",
            "process_module_query_diagnostics_keep_original_source_coordinates",
            "process_module_query_returns_cursor_type_and_goal_through_worker",
            "process_module_query_references_through_worker",
        )
        proof_cases = (
            "attempt_proof_successful_candidate_closes_simple_goal",
            "attempt_proof_envelope_entry_state_carries_goals_and_locals",
            "attempt_proof_bad_selector_yields_empty_entry_state",
            "attempt_proof_bad_candidate_on_unicode_signature_does_not_false_close",
            "attempt_proof_single_line_by_tactic_aligns_and_closes",
            "attempt_proof_entry_selector_accepts_from_scratch_block_where_default_traps",
            "attempt_proof_candidate_list_is_capped_and_does_not_write_files",
            "attempt_proof_ten_candidates_returns_ordered_rows",
            "attempt_proof_global_output_budget_returns_partial_not_attempted_rows",
        )
        batch_cases = (
            (
                "typed_session",
                "process_module_query_batch_returns_diagnostics_and_proof_state_through_worker",
            ),
            (
                "typed_session",
                "declaration_outline_batch_selector_returns_source_order_rows_through_worker",
            ),
            (
                "typed_session",
                "proof_state_in_declaration_renders_locals_pretty_by_default_and_raw_on_opt_out",
            ),
            (
                "typed_session",
                "proof_state_unresolved_after_text_returns_boundary_candidates_through_worker",
            ),
            (
                "typed_session",
                "proof_state_boundary_candidates_report_truncation_through_worker",
            ),
            ("typed_session", "proof_state_entry_selector_renders_pristine_entry_goal"),
            (
                "typed_session",
                "module_query_on_incomplete_closure_skips_body_elaboration",
            ),
            ("session_reuse", "the_snapshot_cache_survives_a_reuse"),
            (
                "session_reuse",
                "a_pooled_sibling_in_another_mode_never_answers_from_the_wrong_environment",
            ),
        )
        verification_cases = (
            "verify_declaration_not_found_carries_similar_declarations",
            "verify_declaration_accepts_closed_theorem_and_rejects_sorry",
            "module_public_section_resolves_to_unique_declaration",
            "verify_declaration_reports_ambiguous_with_candidates",
            "verify_declaration_ambiguous_open_reference_does_not_restart_child",
            "verify_declaration_reports_needs_build_for_unbuilt_import",
            "declaration_verification_batch_returns_ordered_rows_through_worker",
        )
        wide_reuse_cases = (
            "switching_back_to_a_pooled_import_set_does_not_reimport",
            "capacity_one_restores_the_pre_pool_child",
            "a_reused_session_carries_no_state_from_the_previous_one",
            "a_mispredicted_reuse_restarts_before_the_actual_import",
        )
        reuse_cases = (
            *wide_reuse_cases[:3],
            "a_capacity_eviction_clears_the_snapshot_cache",
            "an_alternation_that_fits_the_pool_never_evicts",
            wide_reuse_cases[3],
        )
        staleness_cases = (
            "a_late_extension_registration_never_reaches_a_pooled_environment",
            "a_late_extension_registration_evicts_the_pooled_environment",
            "an_import_that_registers_nothing_leaves_the_pool_intact",
            "capacity_one_is_unaffected_by_staleness_eviction",
            "a_staleness_eviction_does_not_disable_the_reuse_hint",
        )
        audited_filters = [
            "(binary(=pool) & (test(=compatible_session_key_reuses_one_worker)))",
            "(binary(=typed_command) & (test(=typed_json_command_decodes_response)))",
            *(f"(binary(=pool) & (test(={name})))" for name in pool_cases),
            *(f"(binary(=typed_session) & (test(={name})))" for name in meta_cases),
            *(
                f"(binary(={target}) & (test(={name})))"
                for target, name in adapter_cases
            ),
            *(
                f"(binary(=typed_session) & (test(={name})))"
                for name in (*module_query_cases, *proof_cases)
            ),
            *(f"(binary(={target}) & (test(={name})))" for target, name in batch_cases),
            *(
                f"(binary(=session_reuse) & (test(={name})))"
                for name in (*reuse_cases, *staleness_cases)
            ),
            *(
                f"(binary(=typed_session) & (test(={name})))"
                for name in verification_cases
            ),
            "(binary(=typed_session) & (test(=attempt_proof_bad_candidate_returns_diagnostics_and_session_survives)))",
            "(binary(=typed_session) & (test(=process_module_query_batch_obeys_total_budget_without_killing_worker)))",
        ]
        self.assertNotIn("LEAN_RS_TEST_AUDIT_WORKER_START", run_env)
        self.assertEqual([cmd[-1] for cmd, _ in audited_cases], audited_filters)
        self.assertNotIn("LEAN_RS_TEST_WIDE_REUSE_CASE", run_env)
        self.assertNotIn("LEAN_RS_TEST_STALENESS_PROFILE", run_env)
        self.assertNotIn("LEAN_RS_TEST_MODULE_SYNTAX_CASE", run_env)
        for cmd, case_env in audited_cases:
            self.assertEqual(cmd[cmd.index("--retries") + 1], "0")
            wide_case = case_env.get("LEAN_RS_TEST_WIDE_REUSE_CASE")
            expected_wide = next(
                (
                    name
                    for name in wide_reuse_cases
                    if cmd[-1] == f"(binary(=session_reuse) & (test(={name})))"
                ),
                None,
            )
            self.assertEqual(wide_case, expected_wide)
            staleness = any(
                cmd[-1] == f"(binary(=session_reuse) & (test(={name})))"
                for name in staleness_cases
            )
            self.assertEqual(
                case_env.get("LEAN_RS_TEST_STALENESS_PROFILE"),
                "1" if staleness else None,
            )
            module_syntax = (
                "process_module_query_handles_module_system_header_through_worker"
            )
            self.assertEqual(
                case_env.get("LEAN_RS_TEST_MODULE_SYNTAX_CASE"),
                module_syntax
                if cmd[-1] == f"(binary(=typed_session) & (test(={module_syntax})))"
                else None,
            )
            expected_adapter = next(
                (
                    name
                    for target, name in adapter_cases
                    if cmd[-1] == f"(binary(={target}) & (test(={name})))"
                ),
                None,
            )
            self.assertEqual(
                case_env.get("LEAN_RS_TEST_ADAPTER_CASE"), expected_adapter
            )
            self.assertEqual(case_env["LEAN_RS_TEST_SCALAR_PRIVATE_JOB"], "1")
            self.assertEqual(
                json.loads(case_env["LEAN_RS_TEST_SCALAR_FIXTURE"]),
                [str(self.sdk / "fixtures/lean"), 5, 6],
            )
            self.assertEqual(
                case_env["LEAN_RS_PROFILE_FIXTURE_SHA256"], "fixture-digest"
            )
            self.assertNotIn("LEAN_HOST_MCP_WRITER_EXECUTABLE", case_env)
            self.assertEqual(case_env["LEAN_RS_TEST_AUDIT_WORKER_START"], "1")
            self.assertEqual(
                case_env["LEAN_RS_TEST_ORDINARY_TYPED_SESSION"],
                run_env["LEAN_RS_TEST_ORDINARY_TYPED_SESSION"],
            )
        self.assertEqual(
            json.loads(run_env["LEAN_RS_TEST_ORDINARY_TYPED_SESSION"])["json_manifest"],
            str(self.state.with_name("typed-session") / "worker-json-capability.json"),
        )
        remaining = assignments["LEAN_RS_TEST_REMAINING_FILTER"]
        builder_cases = (
            "a_host_handle_built_with_a_broken_import_still_fails_at_open",
            "a_host_handle_built_without_imports_opens_no_session",
            "shims_only_host_handle_skips_user_shared_build",
            "shims_only_host_handle_imports_transitive_lake_package_oleans",
        )
        builder_filter = " | ".join(f"test(={name})" for name in builder_cases)
        scalar_filters = [
            "(binary(=protocol) & (test(=fixture_capability_loads_and_exported_call_succeeds)))",
            "(binary(=supervisor) & (test(=public_fixture_export_call_succeeds)))",
            "(binary(=protocol) & (test(=lean_internal_panic_kills_only_child)))",
            "(binary(=supervisor) & (test(=child_crash_is_typed_and_parent_survives)))",
        ]
        self.assertEqual(
            remaining,
            f"not (package(=lean-rs-worker-child) & ((binary(=capability_builder) & ({builder_filter})) | {selection} | {' | '.join(audited_filters + scalar_filters)}))",
        )

    def test_direct_transitive_replaces_only_the_matching_host_test(self):
        _, output = self.run_typed_selection(
            direct_tests=(qualification.DIRECT_TRANSITIVE_TEST,)
        )
        remaining = dict(line.split("=", 1) for line in output.splitlines())[
            "LEAN_RS_TEST_REMAINING_FILTER"
        ]
        self.assertTrue(
            remaining.endswith(
                " | (package(=lean-rs-host) & (binary(=host_services) & "
                "(test(=import_finds_transitive_lake_package_oleans)))))"
            )
        )
        self.assertEqual(remaining.count("package(=lean-rs-host)"), 1)

    def test_good_pair_replaces_only_its_original_worker_and_host_cases(self):
        _, output = self.run_typed_selection(
            direct_tests=(qualification.DIRECT_GOOD_TEST,)
        )
        remaining = dict(line.split("=", 1) for line in output.splitlines())[
            "LEAN_RS_TEST_REMAINING_FILTER"
        ]
        self.assertIn(
            f"package(=lean-rs-worker-child) & (binary(=session_adapter) & "
            f"(test(={qualification.GOOD_WORKER_TEST})))",
            remaining,
        )
        self.assertIn(
            f"package(=lean-rs-host) & (binary(=host_services) & "
            f"(test(={qualification.DIRECT_GOOD_TEST})))",
            remaining,
        )
        self.assertEqual(remaining.count(qualification.GOOD_WORKER_TEST), 1)
        self.assertNotIn(
            "load_shims_only_succeeds_when_user_shared_facet_does_not_build", remaining
        )
        self.assertNotIn(qualification.DIRECT_TRANSITIVE_TEST, remaining)

    def test_broken_shared_replaces_only_its_original_host_case(self):
        case = qualification.DIRECT_BROKEN_SHARED_TEST
        _, output = self.run_typed_selection(direct_tests=(case,))
        remaining = dict(line.split("=", 1) for line in output.splitlines())[
            "LEAN_RS_TEST_REMAINING_FILTER"
        ]
        self.assertTrue(
            remaining.endswith(
                f" | (package(=lean-rs-host) & (binary(=host_services) & (test(={case})))))"
            )
        )
        self.assertEqual(remaining.count(case), 1)
        self.assertNotIn(qualification.DIRECT_GOOD_TEST, remaining)
        self.assertNotIn(qualification.GOOD_WORKER_TEST, remaining)

    def test_optional_whnf_replaces_only_its_original_host_case(self):
        case = qualification.DIRECT_OPTIONAL_WHNF_TEST
        _, output = self.run_typed_selection(direct_tests=(case,))
        remaining = dict(line.split("=", 1) for line in output.splitlines())[
            "LEAN_RS_TEST_REMAINING_FILTER"
        ]
        self.assertTrue(
            remaining.endswith(
                f" | (package(=lean-rs-host) & (binary(=host_services) & (test(={case})))))"
            )
        )
        self.assertEqual(remaining.count(case), 1)

    def test_direct_transitive_receipt_requires_process_exit(self):
        marker = ((b"direct-host-transitive-passed",),)
        audit = b"51 direct-host-transitive-passed\n"
        with patch.object(qualification.os, "kill", side_effect=ProcessLookupError):
            qualification.check_host_builder_children_closed(audit, marker, prefix=())
        with (
            patch.object(qualification.os, "kill", return_value=None),
            self.assertRaisesRegex(ValueError, "PID 51 remains"),
        ):
            qualification.check_host_builder_children_closed(audit, marker, prefix=())

    def test_typed_group_extra_pool_worker_keeps_state_and_skips_json_smoke(self):
        self.run_typed_selection(extra_json_worker=True)

    def test_json_pool_policy_checks_closure_of_start_without_manifest_grant(self):
        case = "per_worker_rss_policy_invalidates_old_lease_before_work"
        counts = next(
            counts
            for _, name, counts in qualification.ORDINARY_JSON_TESTS
            if name == case
        )
        audit = (
            b"51 worker-started\n51 verified-deployment\n51 authorize-consumer-load\n"
            b"52 worker-started\n"
        )

        def check_pid(pid, _signal):
            if pid == 51:
                raise ProcessLookupError

        with (
            patch.object(qualification.os, "kill", side_effect=check_pid) as probe,
            self.assertRaisesRegex(ValueError, "52 remains; keeping fixture state"),
        ):
            qualification.check_typed_children_closed(
                audit, case=case, expected_counts=counts
            )
        probe.assert_any_call(52, 0)

    def test_meta_def_eq_requires_two_term_grants_from_the_same_worker(self):
        case = "is_def_eq_recognises_equal_terms"
        audit = (
            b"51 worker-started\n51 verified-deployment\n51 authorize-fixed-meta-term\n"
        )
        with (
            patch.object(qualification.os, "kill") as probe,
            self.assertRaisesRegex(
                ValueError, "wrong started worker / authorize-fixed-meta-term counts"
            ),
        ):
            qualification.check_typed_children_closed(
                audit,
                case=case,
                expected_counts=((1, 2),),
                grant_event=qualification.META_TERM_GRANT,
            )
        probe.assert_not_called()

    def test_host_builder_audit_requires_each_pid_and_ordered_scope(self):
        expected = ((b"authorize-builder-good",), (b"authorize-builder-empty",))
        good = (
            b"51 worker-started\n51 verified-deployment\n51 authorize-builder-good\n"
            b"52 worker-started\n52 verified-deployment\n52 authorize-builder-empty\n"
        )
        with patch.object(
            qualification.os, "kill", side_effect=ProcessLookupError
        ) as probe:
            qualification.check_host_builder_children_closed(good, expected)
        self.assertEqual(probe.call_count, 2)
        wrong = good.replace(
            b"52 authorize-builder-empty", b"51 authorize-builder-empty"
        )
        with (
            patch.object(qualification.os, "kill") as probe,
            self.assertRaisesRegex(
                ValueError, "wrong worker count, custody or ordered imports"
            ),
        ):
            qualification.check_host_builder_children_closed(wrong, expected)
        probe.assert_not_called()
        with (
            patch.object(
                qualification.os, "kill", side_effect=[ProcessLookupError, None]
            ),
            self.assertRaisesRegex(ValueError, "PID 52 remains"),
        ):
            qualification.check_host_builder_children_closed(good, expected)

    def test_legacy_provider_preflight_refuses_stale_outputs_and_broken_aliases(self):
        provider = self.target / "provider"
        root = provider / ".lake/build/lib/lean"
        root.parent.mkdir(parents=True)
        qualification.require_no_legacy_provider_shadow(provider)

        root.symlink_to(provider / "missing")
        with self.assertRaisesRegex(ValueError, "would shadow the pinned provider"):
            qualification.require_no_legacy_provider_shadow(provider)
        self.assertTrue(root.is_symlink())
        root.unlink()

        root.mkdir()
        (root / "LeanRsInterop.olean").touch()
        with self.assertRaisesRegex(ValueError, "would shadow the pinned provider"):
            qualification.require_no_legacy_provider_shadow(provider)
        self.assertTrue((root / "LeanRsInterop.olean").is_file())

    def test_foreign_and_signature_cases_get_separate_seals_without_rebuilding_provider(
        self,
    ):
        project = self.sdk / "fixtures/interop-worker"
        project.mkdir(parents=True, exist_ok=True)
        release = self.target
        manifest = self.target / "shim.json"
        regular = "successful_stream_delivers_rows_with_per_stream_sequences"
        missing, wrong = qualification.INTEROP_SIGNATURE_CASES
        foreign = list(qualification.INTEROP_FOREIGN_IMPORT_CASES)
        legacy = qualification.INTEROP_LEGACY_CASE
        assets = qualification.stream_package_root()
        assets.mkdir(parents=True)
        manifest.write_text("{}")
        original_images = [("original", 1, 2, "original-digest")]
        isolated_images = [("isolated", 1, 3, "isolated-digest")]
        pinned = ({"shim_manifest_path": str(manifest), "shim_manifest_json": "{}"}, original_images)
        with (
            patch.object(qualification, "prepare_stream_package", return_value=(assets.stat(), manifest, isolated_images)),
            patch.object(qualification, "run_stream_group") as run,
        ):
            qualification.qualify_stream(
                release, qualification.DARWIN_ARM64_PINS, manifest,
                [regular, foreign[0], missing, legacy, *foreign[1:], wrong], pinned_selection=pinned,
            )
        self.assertEqual(
            [args[3] for args, _ in run.call_args_list],
            [[regular], foreign, (missing,), (wrong,), [legacy]],
        )
        self.assertEqual([kwargs["build_provider"] for _, kwargs in run.call_args_list],
                         [True, False, False, False, False])
        for _, kwargs in run.call_args_list:
            self.assertEqual(kwargs["image_pins"], original_images + isolated_images)
        self.assertFalse(assets.exists())
        assets.mkdir()
        with (
            patch.object(qualification, "prepare_stream_package", return_value=(assets.stat(), manifest, [])),
            patch.object(qualification, "run_stream_group", side_effect=ValueError("first did not close")) as run,
            self.assertRaisesRegex(ValueError, "first did not close"),
        ):
            qualification.qualify_stream(release, qualification.DARWIN_ARM64_PINS, manifest, [missing, wrong])
        run.assert_called_once()
        self.assertTrue(assets.is_dir())

    def test_workspace_uses_the_named_package_and_removes_it_only_after_success(self):
        root = qualification.stream_package_root()
        root.mkdir(mode=0o700)
        owner = root.stat()
        manifest = root / "shim.json"
        manifest.write_text("{}")
        pins = []
        for name in ("host", "interop"):
            image = root / name
            image.write_text(name)
            identity = image.stat()
            pins.append([str(image), identity.st_dev, identity.st_ino, qualification.digest(image)])
        descriptor = {
            "shim_manifest_path": str(manifest), "shim_manifest_json": "{}",
            "artifact_roots": [], "shim_search_paths": [],
        }
        with (
            patch.object(qualification, "prepare_stream_package", return_value=(owner, manifest, pins)),
            patch.object(qualification, "run_stream_group", return_value=descriptor),
        ):
            selection = qualification.qualify_stream(
                self.target, {}, manifest, ["successful_stream_delivers_rows_with_per_stream_sequences"],
                prepared_package=(owner, manifest, pins), retain_for_workspace=True,
            )
        self.assertTrue(root.is_dir())
        env = {"LEAN_RS_TEST_ORDINARY_HOST_SHIMS": selection}
        with (
            patch.dict(os.environ, env),
            patch.object(qualification, "STATE_LOCK", root.parent / ".ordinary.lock"),
            patch("subprocess.run") as run,
        ):
            bad = {**json.loads(selection), "package_identity": [owner.st_dev, owner.st_ino + 1]}
            with patch.dict(os.environ, {"LEAN_RS_TEST_ORDINARY_HOST_SHIMS": json.dumps(bad)}):
                with self.assertRaisesRegex(ValueError, "ownership changed"):
                    qualification.workspace_tests("all()")
            run.assert_not_called()
            # A previously entered workspace run has not confirmed closure.
            marker = root / "workspace-running"
            marker.mkdir()
            with self.assertRaises(FileExistsError):
                qualification.workspace_tests("all()")
            run.assert_not_called()
            marker.rmdir()
            run.return_value.returncode = 0
            qualification.workspace_tests("all()")
            run.assert_called_with(
                ["cargo", "nextest", "run", "--workspace", "--profile", "ci", "-E", "all()"], cwd=self.sdk
            )
        self.assertFalse(root.exists())

    def test_focused_stream_consumes_the_verified_handoff_only_once(self):
        root = qualification.stream_package_root()
        root.mkdir(mode=0o700)
        owner = root.stat()
        manifest = root / "host.json"
        manifest.write_text("{}")
        pins = []
        for name in ("host", "interop"):
            image = root / name
            image.write_text(name)
            info = image.stat()
            pins.append([str(image), info.st_dev, info.st_ino, qualification.digest(image)])
        receipt = {"package_identity": [owner.st_dev, owner.st_ino], "manifest": str(manifest),
                   "manifest_sha256": qualification.digest(manifest), "images": pins}
        note = root / "stream-ready.json"
        note.write_text(json.dumps(receipt))
        with patch.object(qualification, "prepare_stream_package") as build, patch.object(
            qualification, "run_stream_group", side_effect=ValueError("worker closure unknown")
        ) as run:
            manifest.write_text("changed")
            with self.assertRaisesRegex(ValueError, "manifest changed"):
                qualification.qualify_stream(self.target, {}, manifest, ["successful_stream_delivers_rows_with_per_stream_sequences"])
            run.assert_not_called()
            self.assertTrue(note.exists())
            manifest.write_text("{}")
            with self.assertRaisesRegex(ValueError, "closure unknown"):
                qualification.qualify_stream(self.target, {}, manifest, ["successful_stream_delivers_rows_with_per_stream_sequences"])
            self.assertFalse(note.exists())
            with self.assertRaises(FileNotFoundError):
                qualification.qualify_stream(self.target, {}, manifest, ["successful_stream_delivers_rows_with_per_stream_sequences"])
            run.assert_called_once()
            build.assert_not_called()
        self.assertTrue(root.is_dir())

    def test_missing_doctor_exports_audit_requires_admission_without_a_grant(self):
        case = qualification.NO_EXPORTS_CASE
        marker = f"admit-fixed-no-exports-{case}".encode()
        audit = b"51 worker-started\n51 verified-deployment\n51 " + marker + b"\n"
        with patch.object(
            qualification.os, "kill", side_effect=ProcessLookupError
        ) as probe:
            qualification.check_typed_children_closed(
                audit, case=case, expected_counts=((1, 1),), grant_event=marker
            )
        probe.assert_called_once_with(51, 0)
        with (
            patch.object(qualification.os, "kill") as probe,
            self.assertRaisesRegex(ValueError, "wrong started worker"),
        ):
            qualification.check_typed_children_closed(
                audit
                + b"51 authorize-fixed-doctor-doctor_reports_pass_warning_and_error_diagnostics\n",
                case=case,
                expected_counts=((1, 1),),
                grant_event=marker,
            )
        probe.assert_not_called()

    def test_metadata_audit_refuses_a_grant_for_another_interop_case(self):
        for target, name in qualification.INTEROP_METADATA_TESTS:
            with self.subTest(target=target, name=name):
                grant = f"authorize-fixed-metadata-{name}".encode()
                audit = (
                    b"51 worker-started\n51 verified-deployment\n51 " + grant + b"\n"
                )
                with patch.object(
                    qualification.os, "kill", side_effect=ProcessLookupError
                ) as probe:
                    qualification.check_typed_children_closed(
                        audit, case=name, expected_counts=((1, 1),), grant_event=grant
                    )
                probe.assert_called_once_with(51, 0)
                with (
                    patch.object(qualification.os, "kill") as probe,
                    self.assertRaisesRegex(ValueError, "wrong started worker"),
                ):
                    qualification.check_typed_children_closed(
                        audit
                        + b"51 authorize-fixed-stream-malformed_row_json_is_typed\n",
                        case=name,
                        expected_counts=((1, 1),),
                        grant_event=grant,
                    )
                probe.assert_not_called()

    def test_module_query_audit_refuses_an_unexpected_term_grant(self):
        audit = (
            b"51 worker-started\n51 verified-deployment\n"
            b"51 authorize-fixed-module-query\n51 authorize-fixed-meta-term\n"
        )
        with (
            patch.object(qualification.os, "kill") as probe,
            self.assertRaisesRegex(
                ValueError, "wrong started worker / authorize-fixed-module-query counts"
            ),
        ):
            qualification.check_typed_children_closed(
                audit,
                case="process_module_query_returns_diagnostics_through_worker",
                expected_counts=((1, 1),),
                grant_event=qualification.MODULE_QUERY_GRANT,
            )
        probe.assert_not_called()

    def test_cancelled_adapter_requires_ready_only_replacement(self):
        name = "parent_can_cancel_long_worker_request_at_progress_boundary"
        grant = qualification.ADAPTER_IMPORT_GRANT
        audit = (
            b"51 worker-started\n51 verified-deployment\n51 "
            + grant
            + b"\n52 worker-started\n"
        )
        with patch.object(
            qualification.os, "kill", side_effect=ProcessLookupError
        ) as probe:
            qualification.check_typed_children_closed(
                audit, case=name, expected_counts=((2, 1),), grant_event=grant
            )
        self.assertEqual(probe.call_count, 2)
        with (
            patch.object(qualification.os, "kill") as probe,
            self.assertRaisesRegex(ValueError, "wrong started worker"),
        ):
            qualification.check_typed_children_closed(
                audit + b"52 verified-deployment\n52 " + grant + b"\n",
                case=name,
                expected_counts=((2, 1),),
                grant_event=grant,
            )
        probe.assert_not_called()

    def test_mixed_cases_refuse_swapped_grants_on_the_expected_pid(self):
        for case, events in qualification.ORDINARY_MIXED_GRANTS.items():
            with self.subTest(case=case):
                audit = b"51 worker-started\n51 verified-deployment\n" + b"".join(
                    b"51 " + marker + b"\n" for marker in reversed(events)
                )
                with (
                    patch.object(qualification.os, "kill") as probe,
                    self.assertRaisesRegex(ValueError, "grant order"),
                ):
                    qualification.check_typed_children_closed(
                        audit,
                        case=case,
                        expected_counts=((1, 2),),
                        grant_event=events[0],
                    )
                probe.assert_not_called()

    def test_reuse_restart_requires_exact_checks_before_and_after_replacement(self):
        case = "a_mispredicted_reuse_restarts_before_the_actual_import"
        grant = b" " + qualification.IMPORT_GRANT + b"\n"
        audit = (
            b"51 worker-started\n51 verified-deployment\n"
            + (b"51" + grant) * 2
            + b"52 worker-started\n52 verified-deployment\n"
            + (b"52" + grant) * 3
        )
        with (
            patch.object(qualification.os, "kill") as probe,
            self.assertRaisesRegex(ValueError, "wrong grant counts per worker"),
        ):
            qualification.check_typed_children_closed(
                audit,
                case=case,
                expected_counts=((2, 5),),
                grant_event=qualification.IMPORT_GRANT,
            )
        probe.assert_not_called()

    def test_typed_group_with_live_worker_retains_only_typed_state(self):
        self.run_typed_selection(live_pid=True)

    def test_run_typed_requires_emitting_selections_before_any_fixture_work(self):
        argv = ["qualify_metadata.py", "broken-import", "--run-typed"]
        with (
            patch.object(sys, "argv", argv),
            patch("sys.stderr", new_callable=io.StringIO) as error,
            patch.object(qualification, "ordinary_state_lock") as state_lock,
            self.assertRaises(SystemExit) as stopped,
        ):
            qualification.main()
        self.assertEqual(stopped.exception.code, 2)
        self.assertIn("--run-typed requires --emit-host-selection", error.getvalue())
        state_lock.assert_not_called()
        self.launch.assert_not_called()

    def test_linux_broken_import_selects_official_pins_only_for_named_profile(self):
        pins = qualification.verified_platform("Linux", "x86_64", broken_import=True)
        self.assertEqual(
            pins["lean"],
            "db7f559d05795e7af52e42bc966c90240a39dcb9712161c54c97afa546e20c00",
        )
        self.assertEqual(
            pins["lake"],
            "42678ccbc2605e2f892540dc7236e009fdd1224e823d104dfd72725d2f1a52d2",
        )
        self.assertEqual(
            pins["runtime"],
            "e2c403b411497bbc325d0a26bf57ba0132271750bf6d84e32cd61a9db4777252",
        )
        self.assertEqual(pins["runtime_name"], "libleanshared.so")
        self.assertEqual(pins["nextest_runner"], "/usr/bin/env -u LD_LIBRARY_PATH")
        with self.assertRaisesRegex(ValueError, "Linux ordinary dispatch awaits"):
            qualification.verified_platform("Linux", "x86_64")
        self.launch.assert_not_called()

    def test_linux_broken_import_command_passes_selected_profile(self):
        (self.target / "lib/lean").mkdir(parents=True)
        with (
            patch.object(sys, "argv", ["qualify_metadata.py", "broken-import"]),
            patch.object(qualification.platform, "system", return_value="Linux"),
            patch.object(qualification.platform, "machine", return_value="x86_64"),
            patch.dict(os.environ, {}, clear=True),
            patch.object(
                qualification, "installed_lean_release", return_value=self.target
            ) as release,
            patch.object(qualification, "ordinary_state_lock"),
            patch.object(qualification, "qualify_broken_import") as run,
        ):
            qualification.main()
        release.assert_called_once_with(qualification.LINUX_X86_64_BROKEN_IMPORT_PINS)
        run.assert_called_once_with(
            self.target,
            qualification.LINUX_X86_64_BROKEN_IMPORT_PINS,
            emit_host_selection=False,
            run_typed=False,
            run_stream=False,
            bookended_release=True,
            retain_for_workspace=False,
        )
        self.launch.assert_not_called()

    def test_ordinary_bookends_refuse_same_size_release_change_before_selection(self):
        lib = self.target / "lib/lean"
        lib.mkdir(parents=True)
        module = lib / "Nat.olean"
        module.write_bytes(b"good")
        original_inode = module.stat().st_ino

        def run(*_args, **kwargs):
            self.assertTrue(kwargs["bookended_release"])
            self.assertTrue(kwargs["emit_host_selection"])
            module.write_bytes(b"evil")
            print("LEAN_RS_TEST_ORDINARY_HOST_SHIMS=must-not-be-published")

        argv = ["qualify_metadata.py", "broken-import", "--emit-host-selection"]
        with (
            patch.object(sys, "argv", argv),
            patch.object(
                qualification,
                "verified_platform",
                return_value=qualification.DARWIN_ARM64_PINS,
            ),
            patch.object(
                qualification, "installed_lean_release", return_value=self.target
            ),
            patch.object(qualification, "ordinary_state_lock"),
            patch.object(qualification, "qualify_broken_import", side_effect=run),
            patch("sys.stdout", new_callable=io.StringIO) as output,
            patch("sys.stderr", new_callable=io.StringIO) as error,
            self.assertRaises(SystemExit) as stopped,
        ):
            qualification.main()
        self.assertEqual(stopped.exception.code, 1)
        self.assertEqual(module.stat().st_ino, original_inode)
        self.assertEqual(module.stat().st_size, 4)
        self.assertEqual(output.getvalue(), "")
        self.assertIn("installed Lean library tree changed", error.getvalue())
        self.launch.assert_not_called()

    def test_broken_import_refuses_platform_before_creating_state(self):
        argv = ["qualify_metadata.py", "broken-import"]
        with (
            patch.object(sys, "argv", argv),
            patch.object(qualification.platform, "system", return_value="Linux"),
            patch.object(qualification.platform, "machine", return_value="aarch64"),
            patch.dict(os.environ, {}, clear=True),
            patch("sys.stderr", new_callable=io.StringIO) as error,
            patch.object(qualification, "ordinary_state_lock") as state_lock,
            self.assertRaises(SystemExit) as stopped,
        ):
            qualification.main()
        self.assertEqual(stopped.exception.code, 1)
        self.assertIn("awaits verified Lean release", error.getvalue())
        state_lock.assert_not_called()
        self.launch.assert_not_called()


class FixtureSourceTest(unittest.TestCase):
    def test_ordinary_checkout_keeps_tracked_fixture_sources(self):
        app = qualification.SDK / "fixtures/metadata-qualification/app"
        sources = qualification.fixture_sources(qualification.APP_TREE)
        # The ordinary runner uses tracked source in place; the revised sources can
        # differ from the old tree, but must not lose any fixture inputs.
        tracked = subprocess.check_output(["git", "ls-files", "-z", "--", "."], cwd=app)
        self.assertTrue(
            set(sources)
            <= {Path(os.fsdecode(name)) for name in tracked.split(b"\0") if name}
        )


class NativeImageInspectionTest(unittest.TestCase):
    def test_platform_native_names_preserve_macos_and_select_elf(self):
        self.assertEqual(seal.native_name("app", "Darwin"), "libapp.dylib")
        self.assertEqual(seal.native_name("app", "Linux"), "libapp.so")

    def test_elf_dynamic_accepts_only_selected_release_search_path(self):
        release = self.target = Path("/opt/lean/lib/lean")
        output = """
 0x0000000000000001 (NEEDED)             Shared library: [libleanshared.so]
 0x000000000000001d (RUNPATH)            Library runpath: [/opt/lean/lib/lean]
"""
        with patch.object(seal.subprocess, "check_output", return_value=output):
            self.assertEqual(
                seal.elf_dynamic(Path("app.so"), release),
                (["libleanshared.so"], ["/opt/lean/lib/lean"]),
            )

    def test_elf_dynamic_rejects_unknown_dependency_or_search_path(self):
        cases = [
            " 0 (NEEDED) Shared library: [/outside/libbad.so]\n",
            " 0 (NEEDED) Shared library: [libbad.so]\n",
            " 0 (RUNPATH) Library runpath: [$ORIGIN]\n",
            " 0 (RUNPATH) Library runpath: [/opt/lean/lib/lean:]\n",
            (
                " 0 (RPATH) Library rpath: [/opt/lean/lib/lean]\n"
                " 0 (RUNPATH) Library runpath: [/opt/lean/lib/lean]\n"
            ),
            " 0 (FILTER) Filter library: [libredirect.so]\n",
        ]
        for output in cases:
            with (
                self.subTest(output=output),
                patch.object(seal.subprocess, "check_output", return_value=output),
                self.assertRaises(AssertionError),
            ):
                seal.elf_dynamic(Path("app.so"), Path("/opt/lean/lib/lean"))

    def test_elf_exports_do_not_treat_nondefault_version_as_bare(self):
        output = "1 T bare\n2 T defaulted@@V1\n3 T hidden@V1\n"
        with patch.object(seal.subprocess, "check_output", return_value=output):
            self.assertEqual(
                seal.defined_exports(Path("app.so"), "Linux"),
                {"bare", "defaulted", "hidden@V1"},
            )
