"""Pre-execution controls for the ordinary metadata qualification."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from scripts import qualify_metadata as qualification

SEAL_PATH = qualification.APP / "seal.py"
SEAL_SPEC = importlib.util.spec_from_file_location("metadata_qualification_seal", SEAL_PATH)
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
        for name, value in (("SDK", self.sdk), ("TARGET", self.target), ("STATE", self.state),
                            ("SOCKET_ROOT", self.socket_root)):
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
            "files": {str(image): [stat.st_dev, stat.st_ino, qualification.digest(image)]},
            "consumer_manifest": str(manifest),
            "consumer_json": "{}",
            "shim_manifest": "shim.json",
            "shim_json": "{}",
            "shim_roots": [],
            "shim_paths": [],
        }
        self.record.write_text(json.dumps(data))
        expected = {"protocol": 2, "shim_manifest_path": "shim.json", "shim_manifest_json": "{}",
                    "artifact_roots": [], "shim_search_paths": []}
        return owners, expected

    def qualify(self, worker_hash=None, record_hash=None, **options):
        qualification.qualify(
            self.state, self.helper, qualification.digest(self.helper),
            worker_hash or qualification.digest(self.worker),
            record_hash or qualification.digest(self.record), release=self.target,
            approved_control=qualification.digest(self.control), **options
        )

    def test_wrong_worker_or_record_refuses_before_readiness(self):
        wrong = hashlib.sha256(b"substituted").hexdigest()
        for args, refused in [({"worker_hash": wrong}, self.worker), ({"record_hash": wrong}, self.record)]:
            with self.subTest(refused=refused), self.assertRaisesRegex(ValueError, f"SHA-256 mismatch: {refused}"):
                self.qualify(**args)
        self.launch.assert_not_called()

    def test_wrong_control_refuses_before_readiness(self):
        with self.assertRaisesRegex(ValueError, f"SHA-256 mismatch: {self.control}"):
            qualification.qualify(
                self.state, self.helper, qualification.digest(self.helper),
                qualification.digest(self.worker), qualification.digest(self.record),
                release=self.target, approved_control="0" * 64,
            )
        self.launch.assert_not_called()

    def test_wrong_or_indirect_writer_refuses(self):
        approved = qualification.digest(self.helper)
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            qualification.reviewed_writer(self.helper, "0" * 64)
        alias = self.target / "writer-symlink"
        alias.symlink_to(self.helper)
        with self.assertRaisesRegex(ValueError, "expected a canonical executable writer"):
            qualification.reviewed_writer(alias, approved)
        qualification.reviewed_writer(self.helper, approved)
        self.assertFalse((self.state / "lean-host-mcp").exists())
        self.launch.assert_not_called()

    def test_wrong_policy_refuses_before_readiness(self):
        self.record.write_text('{"typed_session": true, "verification": false}')
        with self.assertRaisesRegex(ValueError, "does not declare the ordinary fixture policy"):
            self.qualify()
        self.launch.assert_not_called()

    def test_unsupported_routes_refuse_before_readiness(self):
        for mode in ("typed_session", "disposable_clean", "verification", "unknown_declaration"):
            with self.subTest(mode=mode), self.assertRaisesRegex(ValueError, "unavailable"):
                self.qualify(**{mode: True})
        self.launch.assert_not_called()

    def test_free_space_floor_refuses_before_any_launch(self):
        # Use the real floor check while controlling the reading.
        with patch.object(qualification, "space", REAL_SPACE), patch.object(
            qualification.shutil, "disk_usage", return_value=SimpleNamespace(free=25 * 2**30 - 1)
        ):
            with self.assertRaisesRegex(ValueError, "less than 25 GiB free"):
                self.qualify()
        self.launch.assert_not_called()

    def test_uncertain_readiness_is_not_replayed(self):
        self.valid_record()
        with patch.object(qualification.subprocess, "check_output",
            side_effect=subprocess.CalledProcessError(1, [str(self.worker)]),
        ) as readiness:
            with self.assertRaises(subprocess.CalledProcessError):
                self.qualify()
            self.assertEqual((self.state / "phase").read_text(), "dispatched\n")
            with self.assertRaisesRegex((ValueError, RuntimeError), "already|started|consumed"):
                self.qualify()
        readiness.assert_called_once()
        self.launch.assert_not_called()

    def test_success_collects_exact_registry_before_state_cleanup(self):
        owners, expected = self.valid_record()
        events = self.state / "app/native-events.log"
        prefix = ["verified-deployment", "authorize-consumer-load", "authorize-host-load",
                  "verified-global-interop-initializer", "authorize-Callback-import"]
        next_pid = iter((101, 202))

        def run(command, **_kwargs):
            if command[:3] == ["cargo", "nextest", "run"]:
                pid = next(next_pid)
                with events.open("a") as stream:
                    for event in prefix:
                        stream.write(f"{pid} {event}\n")
            elif command == [self.control, "collect-closed"]:
                self.assertEqual([path.name for path in owners.iterdir()], [".admission.lock"])
            else:
                self.fail(f"unexpected command: {command}")
            return subprocess.CompletedProcess(command, 0)

        with patch.object(qualification.subprocess, "check_output", return_value=json.dumps(expected).encode()) as readiness, \
                patch.object(qualification.subprocess, "run", side_effect=run) as invoked:
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
        self.assertNotIn("LEAN_RS_WORKER_ARTIFACT_IPC_ROOT", readiness.call_args.kwargs["env"])
        self.assertFalse(self.state.exists())
        self.assertFalse(self.socket_root.exists())
        self.assertEqual(invoked.call_args_list[-1].args[0], [self.control, "collect-closed"])

    def test_failed_exact_collection_retains_dispatched_state(self):
        _owners, expected = self.valid_record()
        events = self.state / "app/native-events.log"
        prefix = ["verified-deployment", "authorize-consumer-load", "authorize-host-load",
                  "verified-global-interop-initializer", "authorize-Callback-import"]
        next_pid = iter((303, 404))

        def run(command, **_kwargs):
            if command[:3] == ["cargo", "nextest", "run"]:
                pid = next(next_pid)
                with events.open("a") as stream:
                    for event in prefix:
                        stream.write(f"{pid} {event}\n")
                return subprocess.CompletedProcess(command, 0)
            raise subprocess.CalledProcessError(1, command)

        with patch.object(qualification.subprocess, "check_output", return_value=json.dumps(expected).encode()), \
                patch.object(qualification.subprocess, "run", side_effect=run):
            with self.assertRaises(subprocess.CalledProcessError):
                self.qualify()
        self.assertTrue(self.state.is_dir())
        self.assertEqual((self.state / "phase").read_text(), "dispatched\n")

    def test_reads_only_the_installed_repository_pin(self):
        sdk = self.target / "sdk"
        sdk.mkdir(exist_ok=True)
        (sdk / "lean-toolchain").write_text("leanprover/lean4:v4.36.0\n")
        elan = self.target / "elan"
        release = elan / "toolchains/leanprover--lean4---v4.36.0"
        release.mkdir(parents=True)
        with patch.object(qualification, "SDK", sdk), patch.dict(os.environ, {"ELAN_HOME": str(elan)}, clear=True), \
                patch.object(qualification, "check_file") as check:
            self.assertEqual(qualification.installed_lean_release(), release)
            self.assertEqual([call.args[0] for call in check.call_args_list],
                             [release / "bin/lean", release / "bin/lake", release / "lib/lean/libleanshared.dylib"])
            with patch.dict(os.environ, {"LEAN_SYSROOT": str(sdk)}):
                with self.assertRaisesRegex(ValueError, "inherited LEAN_SYSROOT differs"):
                    qualification.installed_lean_release()
            release.rmdir()
            with self.assertRaises(FileNotFoundError):
                qualification.installed_lean_release()
        self.launch.assert_not_called()

    def test_linux_refuses_without_verified_primary_artifact_pins(self):
        with self.assertRaisesRegex(ValueError, "awaits verified Lean release, helper and worker artifact pins"):
            qualification.verified_platform("Linux", "x86_64")
        self.launch.assert_not_called()

    def test_platform_profiles_select_only_the_reviewed_nextest_loader_cleanup(self):
        cases = [
            (qualification.DARWIN_ARM64_PINS,
             "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
             "/usr/bin/env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH"),
            (qualification.LINUX_X86_64_NEXTEST_RUNNER,
             "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
             "/usr/bin/env -u LD_LIBRARY_PATH"),
        ]
        for profile, variable, command in cases:
            with self.subTest(variable=variable):
                env = {"PATH": "/usr/bin", "LEAN_SYSROOT": "/selected"}
                qualification.select_nextest_runner(env, profile)
                self.assertEqual(env, {
                    "PATH": "/usr/bin", "LEAN_SYSROOT": "/selected", variable: command,
                })

    def test_platform_profile_refuses_inherited_target_runner_override(self):
        variable = qualification.LINUX_X86_64_NEXTEST_RUNNER["nextest_runner_variable"]
        env = {variable: "/unreviewed/runner"}
        with self.assertRaisesRegex(ValueError, "duplicate target runner entry"):
            qualification.select_nextest_runner(env, qualification.LINUX_X86_64_NEXTEST_RUNNER)
        self.assertEqual(env[variable], "/unreviewed/runner")
        self.launch.assert_not_called()

    def test_linux_cli_refuses_inherited_loader_override_before_platform_or_state(self):
        argv = ["qualify_metadata.py", "--writer", str(self.helper),
                "--writer-sha256", qualification.digest(self.helper), "prepare"]
        with patch.object(sys, "argv", argv), \
                patch.object(qualification.platform, "system", return_value="Linux"), \
                patch.dict(os.environ, {"LD_LIBRARY_PATH": "/unreviewed"}, clear=True), \
                patch.object(qualification, "verified_platform") as platform_profile, \
                patch.object(qualification, "ordinary_state_lock") as state_lock, \
                self.assertRaises(SystemExit):
            qualification.main()
        platform_profile.assert_not_called()
        state_lock.assert_not_called()
        self.launch.assert_not_called()

    def test_linux_cli_refuses_before_state_lock_or_dispatch(self):
        argv = ["qualify_metadata.py", "--writer", str(self.helper),
                "--writer-sha256", qualification.digest(self.helper), "prepare"]
        with patch.object(sys, "argv", argv), \
                patch.object(qualification.platform, "system", return_value="Linux"), \
                patch.object(qualification.platform, "machine", return_value="x86_64"), \
                patch.dict(os.environ, {}, clear=True), \
                patch("sys.stderr", new_callable=io.StringIO) as error, \
                patch.object(qualification, "ordinary_state_lock") as state_lock, \
                self.assertRaises(SystemExit) as stopped:
            qualification.main()
        self.assertEqual(stopped.exception.code, 1)
        self.assertIn("awaits verified Lean release", error.getvalue())
        state_lock.assert_not_called()
        self.launch.assert_not_called()

    def test_broken_import_runs_exact_case_with_no_helper_then_cleans(self):
        state = self.state.with_name("broken-import")
        self.worker.chmod(0o500)
        seen = []

        def run(command, *, cwd, env, check):
            seen.append((command, cwd, dict(env), check))
            if command[-1] == "--broken-import":
                (state / "deployment.json").write_text("{}")

        with patch.object(qualification.shutil, "which", return_value="/reviewed/gtimeout"), \
                patch.object(qualification.subprocess, "run", side_effect=run):
            qualification.qualify_broken_import(self.target, qualification.DARWIN_ARM64_PINS)

        self.assertEqual(len(seen), 4)
        for command, _, env, check in seen:
            self.assertEqual(command[:4], ["/reviewed/gtimeout", "--signal=TERM", "--kill-after=15s", "8m"])
            self.assertNotIn("LEAN_HOST_MCP_WRITER_EXECUTABLE", env)
            self.assertNotIn("LEAN_RS_METADATA_AUDIT_PATH", env)
            self.assertTrue(check)
        command, cwd, env, _ = seen[-1]
        self.assertEqual(cwd, self.sdk)
        self.assertEqual(command[-2:], ["-E", f"test(={qualification.BROKEN_IMPORT_TEST})"])
        self.assertEqual(command[command.index("--retries") + 1], "0")
        self.assertEqual(env["CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER"],
                         qualification.DARWIN_ARM64_PINS["nextest_runner"])
        self.assertEqual(json.loads(env["LEAN_RS_TEST_BROKEN_IMPORT"])["project"], str(state / "lean"))
        self.assertFalse(state.exists())

    def test_broken_import_failure_retains_owned_state_and_refuses_replay(self):
        state = self.state.with_name("broken-import")
        with patch.object(qualification.shutil, "which", return_value="/reviewed/gtimeout"), \
                patch.object(qualification.subprocess, "run",
                             side_effect=subprocess.CalledProcessError(124, "timeout")) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                qualification.qualify_broken_import(self.target, qualification.DARWIN_ARM64_PINS)
            self.assertTrue(state.is_dir())
            with self.assertRaises(FileExistsError):
                qualification.qualify_broken_import(self.target, qualification.DARWIN_ARM64_PINS)
        run.assert_called_once()

    def test_broken_import_refuses_platform_before_creating_state(self):
        argv = ["qualify_metadata.py", "broken-import"]
        with patch.object(sys, "argv", argv), \
                patch.object(qualification.platform, "system", return_value="Linux"), \
                patch.object(qualification.platform, "machine", return_value="x86_64"), \
                patch.dict(os.environ, {}, clear=True), \
                patch("sys.stderr", new_callable=io.StringIO) as error, \
                patch.object(qualification, "ordinary_state_lock") as state_lock, \
                self.assertRaises(SystemExit) as stopped:
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
        self.assertTrue(set(sources) <= {Path(os.fsdecode(name)) for name in tracked.split(b"\0") if name})


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
            self.assertEqual(seal.elf_dynamic(Path("app.so"), release),
                             (["libleanshared.so"], ["/opt/lean/lib/lean"]))

    def test_elf_dynamic_rejects_unknown_dependency_or_search_path(self):
        cases = [
            " 0 (NEEDED) Shared library: [/outside/libbad.so]\n",
            " 0 (NEEDED) Shared library: [libbad.so]\n",
            " 0 (RUNPATH) Library runpath: [$ORIGIN]\n",
            " 0 (RUNPATH) Library runpath: [/opt/lean/lib/lean:]\n",
            " 0 (RPATH) Library rpath: [/opt/lean/lib/lean]\n"
            " 0 (RUNPATH) Library runpath: [/opt/lean/lib/lean]\n",
            " 0 (FILTER) Filter library: [libredirect.so]\n",
        ]
        for output in cases:
            with self.subTest(output=output), patch.object(seal.subprocess, "check_output", return_value=output):
                with self.assertRaises(AssertionError):
                    seal.elf_dynamic(Path("app.so"), Path("/opt/lean/lib/lean"))

    def test_elf_exports_do_not_treat_nondefault_version_as_bare(self):
        output = "1 T bare\n2 T defaulted@@V1\n3 T hidden@V1\n"
        with patch.object(seal.subprocess, "check_output", return_value=output):
            self.assertEqual(seal.defined_exports(Path("app.so"), "Linux"),
                             {"bare", "defaulted", "hidden@V1"})
