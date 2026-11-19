"""Pre-execution controls for the named metadata qualification."""
import hashlib
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
                        qualify_metadata.qualify(root, root / "writer", b"", worker_hash, record_hash)
                    self.assertEqual(str(error.exception), f"SHA-256 mismatch: {refused}")
                    launch.assert_not_called()
