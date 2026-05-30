---
name: bump-toolchain
description: Add a Lean release to the supported toolchain window. Use when bumping the Lean version, extending the supported window, or adding a Lean toolchain to lean-rs.
---

# Bump the supported Lean toolchain window

Follow [`docs/bump-toolchain.md`](../../../docs/bump-toolchain.md) exactly — it is the source of truth. The single
source of truth for the window itself is `crates/lean-rs-sys/src/supported.rs`. Summary of the ritual:

1. `elan toolchain install leanprover/lean4:vX.Y.Z` (~500 MB; skip if installed).
2. Capture the `lean.h` SHA-256 and compare it to `SUPPORTED_TOOLCHAINS` in `crates/lean-rs-sys/src/supported.rs`.
   - **Digest matches** an existing entry → append `"X.Y.Z"` to that entry's `versions`, skip to step 4 (the common
     case).
   - **New digest** → do step 3 first.
3. New digest only — both must pass:
   - `scripts/check-lean-header.sh <existing-version> X.Y.Z` (empty = layouts unchanged).
   - `scripts/check-lean-symbols.sh X.Y.Z` (empty = every `REQUIRED_SYMBOLS` entry resolves).
4. Update `crates/lean-rs-sys/src/supported.rs`, then mirror the entry in `crates/lean-rs-sys/digests/manifest.json` and
   `docs/version-matrix.md`.
5. Add `"X.Y.Z"` to the matrix in `.github/workflows/ci.yml`; if it is the new head, also bump the head version in
   `.github/workflows/sanitizer.yml`.
6. Run `scripts/test-all-toolchains.sh` (per-version pass/fail sweep).
7. Commit: `Add Lean X.Y.Z to the supported toolchain window`. The PR body records the new digest, the step-6 summary,
   and any `missing_symbols` changes with rationale.

If a bump fails, do **not** add brittle version-specific wrappers or allowlists — consult the "When the bump fails"
table in `docs/bump-toolchain.md` and act per the symptom.
