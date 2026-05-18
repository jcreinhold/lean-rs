# Changelog

All notable changes to the four published `lean-rs` workspace crates are recorded here. The
format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); each crate version is
governed by Cargo's `0.x` semver. Items inside `pub(crate)` modules are not part of the public
API and are excluded from this log.

The supported Lean toolchain range, Rust MSRV, and tested platforms for each release are recorded
in [`docs/version-matrix.md`](docs/version-matrix.md); release-time procedure is in
[`docs/release.md`](docs/release.md).

## [Unreleased] — RD-2026-05-18-001 split

The opinionated theorem-prover-host stack (`LeanHost` / `LeanCapabilities` / `LeanSession` and
their elaboration / evidence / meta / pool surfaces) split out of `lean-rs` into the new sibling
crate **`lean-rs-host`** per `RD-2026-05-18-001`. The L1 FFI primitive crate `lean-rs` no longer
ships those types; it now matches the (β)-binding norm (the OCaml-shaped pattern every
mainstream Rust ↔ GC-language binding follows). Downstream consumers that want only the typed
FFI surface depend on `lean-rs` and write their own `@[export]` Lean shims; consumers that want
the curated theorem-prover-host capability stack add `lean-rs-host`. The four-crate publish
order is `lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`.

## [0.1.0] — 2026-05-18

First public release of the three crates. Crate-publish order was load-bearing: `lean-rs-sys`
first, then `lean-toolchain`, then `lean-rs`.

### `lean-rs-sys` 0.1.0

Initial release. Raw FFI bindings for the Lean 4 C ABI, published per `RD-2026-05-17-005`.

- Opaque public types. `lean_object` is `[u8; 0]` plus phantom markers (`!Send + !Sync + !Unpin`);
  the layout struct `LeanObjectRepr` is `pub(crate)`. Downstream code reaches refcount, tag, and
  payload state only through this crate's `pub unsafe fn` helpers; reading struct fields is not
  available.
- `# Safety` discipline. Every `pub unsafe fn` (99 of them across `array`, `closure`, `ctor`,
  `external`, `io`, `object`, `refcount`, `scalar`, `string`) carries a `# Safety` section naming
  the precondition. Every `unsafe { ... }` block carries a `// SAFETY:` comment restating the
  specific invariant. The lint at `crates/lean-rs-sys/tests/safety_grep.rs` enforces presence.
- Pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, alongside the extern declarations
  for the matching category (refcount, string, array, …).
- `REQUIRED_SYMBOLS` allowlist — 87 `LEAN_EXPORT`'d symbol names this crate's `extern "C"` blocks
  declare; `tests/linkage.rs` resolves every entry against `libleanshared` at link time.
- `EXPECTED_HEADER_DIGEST` (SHA-256 of `lean.h` the crate was authored against) checked by
  `build.rs` against the discovered header at build time. Mismatch fails the build with bounded
  diagnostics naming both digests and the discovered header path.
- Features: `dynamic` + `mimalloc` (default), `static` (opt-in; selecting it requires extending the
  link set beyond what `lean.h` alone demands — see `build.rs`).

Known gaps:

- Lean's header layout (`LeanObjectRepr` field order) is intentionally **not** part of this crate's
  public semver. Layout-tracking updates to track a new Lean point release are minor bumps; the
  opaque public types are unchanged.
- Windows is unsupported (`docs/architecture/02-versioning-and-compatibility.md`).

### `lean-toolchain` 0.1.0

Initial release. Lean 4 toolchain discovery, fingerprint, fixture digest, link diagnostics, and
reusable `build.rs` helpers for downstream embedders.

- Typed `ToolchainFingerprint` carrying the active Lean version, header path, and header digest,
  plus the `LAKE_FIXTURE_DIGEST` constant covering the in-tree fixture package.
- Pass-through re-exports for `LEAN_HEADER_DIGEST`, `LEAN_HEADER_PATH`, `LEAN_VERSION` from
  `lean-rs-sys`, so a downstream `build.rs` can read the toolchain pins without taking a direct
  `lean-rs-sys` dependency.
- `required_symbols()` returns `lean_rs_sys::REQUIRED_SYMBOLS` directly, keeping the allowlist in
  one place.
- Layered link diagnostics that locate the active `libleanshared` and emit bounded error messages
  when a required symbol is missing or the header digest does not match.

### `lean-rs` 0.1.0

Initial release. The safe front door of the workspace: runtime initialization, owned and borrowed
object handles (internal), typed first-order ABI conversions (internal), compiled module loading,
typed exported function calls, semantic handles, bounded `MetaM` services, batching, and session
pooling.

- Single `'lean` lifetime cascade. `LeanRuntime::init` returns a token whose borrow guards every
  downstream handle (`LeanHost`, `LeanCapabilities`, `LeanSession`, `LeanLibrary`, `LeanModule`,
  `LeanExported`, the four semantic handles, `SessionPool`). Compile-fail tests under
  `tests/compile_fail/` pin the invariants: handles cannot outlive their runtime borrow; runtime,
  session, and handles are `!Send + !Sync`.
- Curated public surface — three publicly visible modules (`module`, `host`, `error`) plus two
  `pub(crate)` infrastructure modules (`runtime`, `abi`) per `RD-2026-05-17-004`. Boundaries are
  policed by `pub(crate)` rather than crate splits, so they can be reorganized without semver
  breakage. Classification table at `docs/architecture/03-host-api.md`; per-crate baselines under
  `docs/api-review/`.
- Typed exported function calls. `LeanExported<'lean, 'lib, Args, R>` per `RD-2026-05-17-007`
  collapsed the prompt-08 arity family into a single generic over a sealed `LeanArgs` trait, an
  `R: DecodeCallResult` return type, and a `LeanAbi` per-type C-ABI representation. `LeanIo<T>`
  expresses the pure-vs-IO return-shape distinction at the type level.
- Two-variant `LeanError` per `RD-2026-05-17-006` — `Lean(LeanException)` for Lean-side exceptions
  and `Host(HostFailure { stage: HostStage })` for host-side setup failures. Structural message
  bounding keeps diagnostics fixed-size. The `LeanDiagnosticCode` projection layered on top
  (`OBSERVABILITY-DIAGNOSTICS`) gives callers a stable caller-facing taxonomy.
- Bulk and pool operations are methods on `LeanSession` rather than a sibling module, with
  `SessionPool` for fixed-cardinality reuse.
- Bounded `MetaM` capability surfaced at `lean_rs::host::meta::*` (sub-module path; opt-in per
  `RD-2026-05-17-004` Decision 1) — `LeanMetaService`, `LeanMetaResponse`, `LeanMetaOptions`, and
  the three pinned constructors `infer_type` / `whnf` / `heartbeat_burn`.
- Structured diagnostics — every error-bearing public type projects to `LeanDiagnosticCode` via
  `.code()`. The crate emits `tracing` spans against the `lean_rs` target; in-process tests can
  capture them via `DiagnosticCapture`. No subscriber is installed by the crate; downstream
  consumers attach their own. See `docs/diagnostics.md`.
- `LEAN_RS_NUM_THREADS` environment variable: when set to a positive integer before the first
  `LeanRuntime::init` call, pins the Lean task manager worker count for the lifetime of the
  process. Unset or invalid values fall back to Lean's compiled-in default (typically one worker
  per core) with a `tracing::warn!` for invalid values. Set this when several Lean-using
  processes run side by side to avoid oversubscribing cores. See `docs/architecture/04-concurrency.md`
  and `docs/testing.md`.

Known gaps:

- Windows is unsupported (`docs/architecture/02-versioning-and-compatibility.md`).
- The `BATCHING-SESSION-REUSE` policy is the initial cut; cardinality-limit policy and back-pressure
  shapes may evolve in subsequent `0.x` minors. See the contract's Caveats in
  `prompts/lean-rs/00-current-state.md` for the current bounds.
- `cargo-public-api` is not yet wired into CI (audit at `docs/api-review.md`); diffing against the
  baselines under `docs/api-review/` is a developer-side step until added.
- The `fuzzing` feature opens a narrow set of `pub(crate)` ABI decoders as `pub fn` entry points
  for the in-tree `fuzz/` crate (cargo-fuzz, nightly-only). It is **not** semver-stable and is
  intentionally invisible in the published docs.

[0.1.0]: https://github.com/jcreinhold/lean-rs/releases/tag/v0.1.0
