"""Pre-execution controls for the named metadata qualification."""
import hashlib
import os
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import qualify_metadata


class MetadataQualificationTest(unittest.TestCase):
    def test_mismatched_worker_or_record_cannot_start_readiness(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(qualify_metadata, "space"):
            root = Path(tmp).resolve()
            worker = root / "target/debug/metadata-worker"
            record = root / "app/deployment.json"
            for path in (worker, record):
                path.parent.mkdir(parents=True)
                path.write_bytes(b"substituted")
            actual = qualify_metadata.digest(worker)
            approved = hashlib.sha256(b"reviewed").hexdigest()
            for worker_hash, record_hash, refused in [(approved, actual, worker), (actual, approved, record)]:
                with self.subTest(refused=refused), patch("subprocess.Popen") as launch:
                    with self.assertRaises(ValueError) as error:
                        qualify_metadata.qualify(root, b"", approved, worker_hash, record_hash)
                    self.assertEqual(str(error.exception), f"SHA-256 mismatch: {refused}")
                    launch.assert_not_called()

    def test_only_reviewed_writer_bytes_reach_the_private_copy(self):
        with tempfile.TemporaryDirectory() as tmp, patch("subprocess.Popen") as launch:
            root = Path(tmp).resolve()
            source = root / "source"
            source.write_bytes(b"local writer")
            source.chmod(0o500)
            approved = qualify_metadata.digest(source)
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                qualify_metadata.reviewed_writer(source, "0" * 64)
            data = qualify_metadata.reviewed_writer(source, approved)
            source.unlink()
            source.write_bytes(b"substitute")
            private = qualify_metadata.private_writer(root, data, approved)
            self.assertEqual(private.read_bytes(), b"local writer")
            private.unlink()
            private.write_bytes(b"substitute")
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                qualify_metadata.private_writer(root, data, approved)
            launch.assert_not_called()

    def test_typed_app_matches_its_pinned_git_tree(self):
        app = qualify_metadata.SDK / "fixtures/metadata-qualification/app"
        sources = qualify_metadata.fixture_sources(qualify_metadata.APP_TREE)
        tracked = subprocess.check_output(["git", "ls-files", "-z", "--", "."], cwd=app)
        self.assertEqual(set(sources), {Path(os.fsdecode(name)) for name in tracked.split(b"\0") if name})
        for name, expected in sources.items():
            with self.subTest(name=name):
                self.assertEqual((app / name).read_bytes(), expected)

    def test_wrong_policy_and_substituted_source_refuse_before_readiness(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(qualify_metadata, "space"):
            root = Path(tmp).resolve()
            worker = root / "target/debug/metadata-worker"
            worker.parent.mkdir(parents=True)
            worker.write_bytes(b"worker")
            app = root / "app"
            app.mkdir()
            record = app / "deployment.json"
            record.write_text('{"typed_session": true}')
            (app / "build.rs").write_bytes(b"substituted")
            with patch.object(qualify_metadata, "fixture_sources", return_value={Path("build.rs"): b"reviewed"}), \
                    patch("subprocess.Popen") as launch:
                for typed_session, message in [(False, "different fixture policy"), (True, "source differs")]:
                    with self.subTest(typed=typed_session), self.assertRaisesRegex(ValueError, message):
                        qualify_metadata.qualify(root, b"", "", qualify_metadata.digest(worker),
                                                 qualify_metadata.digest(record), typed_session)
                launch.assert_not_called()
