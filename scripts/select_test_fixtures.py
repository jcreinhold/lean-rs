#!/usr/bin/env python3
"""Emit checked test-only fixture selections as KEY=VALUE lines.

Run after the existing Lake builds, with all three roots mode 0700. The caller
owns this checkout and keeps it and the selected toolchain unchanged until its
Rust tests exit. This is trusted job ownership, not a same-user writer lock.
No shell code is emitted. A failed selection emits no environment variables.
"""
import hashlib
import json
import os
import stat
import sys
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parent.parent


def select(relative, library):
    root = WORKSPACE / relative
    if root.resolve(strict=True) != root:
        raise SystemExit("fixture root must not be aliased")
    metadata = root.stat()
    if metadata.st_uid != os.getuid() or metadata.st_mode & 0o7777 != 0o700:
        raise SystemExit("fixture must be private to this CI job")
    suffix = "dylib" if sys.platform == "darwin" else "so"
    image = root / ".lake/build/lib" / f"{library}.{suffix}"
    if image.resolve(strict=True) != image:
        raise SystemExit("fixture image must not be aliased")
    selected = image.stat()
    if not stat.S_ISREG(selected.st_mode) or selected.st_size > 1024**3:
        raise SystemExit("fixture must be a regular file of at most 1 GiB")
    with image.open("rb") as source:
        before = os.fstat(source.fileno())
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_size > 1024**3
            or (before.st_dev, before.st_ino) != (selected.st_dev, selected.st_ino)
        ):
            raise SystemExit("fixture identity changed before reading")
        digest = hashlib.sha256()
        remaining = before.st_size
        while chunk := source.read(65536):
            remaining -= len(chunk)
            if remaining < 0:
                raise SystemExit("fixture grew while selecting")
            digest.update(chunk)
        after = os.fstat(source.fileno())
    def identity(st):
        return st.st_dev, st.st_ino, st.st_size, st.st_mtime_ns, st.st_ctime_ns

    if identity(before) != identity(after) or identity(after) != identity(image.stat()):
        raise SystemExit("fixture changed while selecting")
    return root, image, before.st_dev, before.st_ino, digest.hexdigest()


# Compile-time selection and runtime marker both belong to this job.
# This is exclusive job ownership, not managed-reader or hostile-writer protection.
scalar = select("fixtures/lean", "liblean__rs__fixture_LeanRsFixture")
interop = select("crates/lean-rs/shims/lean-rs-interop-shims", "liblean__rs__interop__shims_LeanRsInterop")
consumer = select("fixtures/interop-shims", "liblean__rs__interop__consumer_LeanRsInteropConsumer")
sys.stdout.write("LEAN_RS_TEST_SCALAR_PRIVATE_JOB=1\n")
sys.stdout.write(
    "LEAN_RS_TEST_SCALAR_FIXTURE=" + json.dumps([str(scalar[0]), scalar[2], scalar[3]]) + "\n"
)
sys.stdout.write("LEAN_RS_PROFILE_FIXTURE_SHA256=" + scalar[4] + "\n")
sys.stdout.write("LEAN_RS_TEST_CALLBACK_PRIVATE_JOB=1\n")
for selected, selection_key, digest_key in [
    (interop, "LEAN_RS_TEST_CALLBACK_INTEROP", "LEAN_RS_INTEROP_IMAGE_SHA256"),
    (consumer, "LEAN_RS_TEST_CALLBACK_CONSUMER", "LEAN_RS_CALLBACK_CONSUMER_SHA256"),
]:
    sys.stdout.write(
        selection_key + "=" + json.dumps([str(selected[1]), selected[2], selected[3]]) + "\n"
    )
    sys.stdout.write(digest_key + "=" + selected[4] + "\n")
