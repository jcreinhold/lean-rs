---
name: bump-toolchain
description: Add a Lean release to the supported toolchain window. Use when bumping the Lean version, extending the supported window, or adding a Lean toolchain to lean-rs.
---

# Bump the supported Lean toolchain window

The window itself lives in `crates/lean-rs-abi/src/supported.rs`; the procedure and failure modes are documented in
[`docs/bump-toolchain.md`](../../../docs/bump-toolchain.md). Steps:

1. `elan toolchain install leanprover/lean4:vX.Y.Z` (skip if installed).
2. Capture the `lean.h` SHA-256 and compare it to the digests in `SUPPORTED_TOOLCHAINS`.
   - **Digest matches** an existing entry (the common case) → append `"X.Y.Z"` to that entry's `versions`; skip to
     step 4.
   - **New digest** → do step 3 first.
3. New digest only — both must pass:
   - `scripts/check-lean-header.sh <existing-version> X.Y.Z` (empty = layouts unchanged).
   - `scripts/check-lean-symbols.sh X.Y.Z` (empty = every `REQUIRED_SYMBOLS` entry resolves).
4. Update `crates/lean-rs-abi/src/supported.rs`, then mirror it in `crates/lean-rs-sys/digests/manifest.json` and
   `docs/version-matrix.md`.
5. Add `"X.Y.Z"` to the matrix in `.github/workflows/ci.yml`; if it is the new head, also bump the head version in
   `.github/workflows/sanitizer.yml`.
6. Run the cheap local check per `docs/bump-toolchain.md` step 6: select `vX.Y.Z`, run
   `cargo nextest run -p lean-rs-abi -p lean-toolchain`, then restore the override. The full
   `scripts/test-all-toolchains.sh` sweep runs on CI (`full_matrix` dispatch), not locally.
7. Commit as `Add Lean X.Y.Z to the supported toolchain window`. Record the new digest and the step-6 summary in the
   commit message, plus any `missing_symbols` changes with rationale.

If a bump fails, do **not** add version-specific wrappers or allowlists — consult the "When the bump fails" table in
`docs/bump-toolchain.md` and act per the symptom.
