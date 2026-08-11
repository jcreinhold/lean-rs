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
5. Add `"X.Y.Z"` to the matrix in `.github/workflows/ci.yml`. **If it is the new head, move every head reference** —
   a stale one splits the toolchain and fails CI in ways that only surface after push (see the failure modes in
   `docs/bump-toolchain.md` step 5):
   - `.github/workflows/sanitizer.yml`, `release-recover.yml`, `compile-fail.yml`: `LEAN_VERSION_HEAD`.
   - `.github/workflows/release.yml`: `LEAN_VERSION_HEAD` **and** the `verify` matrix's `lean_version`.
   - `.github/workflows/ci.yml`: the head-gated `if: … matrix.lean_version == '<old head>'` steps (actionlint,
     package/docs.rs simulation, nightly install, public-API diff).
   - `scripts/prerelease.sh`: `DEFAULT_LEAN_VERSION` (mirrors `release.yml`'s head).
   - **Committed `lean-toolchain` pins** (root, the three `crates/**/shims/**`, `fixtures/lean`,
     `fixtures/interop-shims`, `templates/shipped-lean-crate/lean`, `formal/RuntimeModel`): `sanitizer.yml` and
     `compile-fail.yml` have no repin step, so a stale pin makes the loader fail with
     `libleanshared.so: cannot open shared object file`.
   - Head/window references in `README.md`, `crates/lean-rs-sys/README.md`, `crates/lean-rs-host/README.md`,
     `docs/release.md`, and `docs/architecture/02-versioning-and-compatibility.md`.
   Grep for the old head string repo-wide (excluding `.git/`) to confirm nothing is left. `scripts/prerelease.sh`
   runs a "Toolchain head consistency" gate that derives the head from `SUPPORTED_TOOLCHAINS` and fails on any stale
   reference — run it before committing.
6. Run the cheap local check per `docs/bump-toolchain.md` step 6: select `vX.Y.Z`, run
   `cargo nextest run -p lean-rs-abi -p lean-toolchain`, then restore the override. The full
   `scripts/test-all-toolchains.sh` sweep runs on CI (`full_matrix` dispatch), not locally.
7. Commit as `Add Lean X.Y.Z to the supported toolchain window`. Record the new digest and the step-6 summary in the
   commit message, plus any `missing_symbols` changes with rationale.

If a bump fails, do **not** add version-specific wrappers or allowlists — consult the "When the bump fails" table in
`docs/bump-toolchain.md` and act per the symptom.
