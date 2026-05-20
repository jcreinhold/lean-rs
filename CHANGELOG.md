# Changelog

All notable changes to the four published `lean-rs` workspace crates are recorded here. The
format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); each crate version is
governed by Cargo's `0.x` semver. Items inside `pub(crate)` modules are not part of the public
API and are excluded from this log.

The supported Lean toolchain range, Rust MSRV, and tested platforms for each release are recorded
in [`docs/version-matrix.md`](docs/version-matrix.md); release-time procedure is in
[`docs/release.md`](docs/release.md).

## [Unreleased]

_Nothing yet._

## [0.1.1] — 2026-05-20

Hardening release for the reusable Lean/Rust interop stack. This release keeps the same Lean
toolchain window as 0.1.0: **4.26.0 through 4.29.1**.

### `lean-rs-sys` 0.1.1

- Added the internal `metadata-only` feature so `lean-toolchain` can depend on build-time Lean
  metadata without linking downstream `build.rs` binaries to `libleanshared`.

### `lean-toolchain` 0.1.1

- Added `build_lake_target(project_root, target_name)` and
  `build_lake_target_quiet(project_root, target_name)` for Lake shared-library targets. The
  helpers hide Lake output naming, cache hits, cache misses, and Cargo rerun directives behind
  typed `LinkDiagnostics`.
- Added `emit_lean_link_directives_checked()` for callers that want typed link diagnostics rather
  than warning-only output.

### `lean-rs` 0.1.1

- Added the L1 callback registry:
  `LeanCallbackHandle<P: LeanCallbackPayload>`, `LeanProgressTick`, `LeanStringEvent`,
  `LeanCallbackFlow`, and `LeanCallbackStatus`. Lean can call Rust through opaque handles and
  crate-owned trampolines without exposing public raw callback pointers. Callback payloads are a
  sealed family; downstream crates can use the supported tick and string payloads but cannot add
  arbitrary payload ABI shapes.
- Bundled the generic `lean-rs-interop-shims` Lake package under the crate so downstream L1
  consumers do not depend on in-tree development paths.
- Added the downstream interop example and recipe covering Rust-to-Lean exported calls plus
  Lean-to-Rust callbacks without `lean-rs-host`.
- Added the string streaming callback example and recipe, showing Lean-to-Rust JSONL-like row
  streaming through `LeanCallbackHandle<LeanStringEvent>` without making `lean-rs` own the row
  schema.

### `lean-rs-host` 0.1.1

- Added cooperative cancellation (`LeanCancellationToken`) and structured progress
  (`LeanProgressSink`, `LeanProgressEvent`) to long-running host-session operations.
- Added source-range lookup, filtered declaration listing, `is_def_eq`, and the three bulk
  declaration-property methods.
- Bundled the host and generic shim packages under the crate. Consumers no longer add
  `lean_rs_host_shims` or `lean_rs_interop_shims` to their own `lakefile.lean`; the host loader
  builds and opens the bundled shims on demand.
- Added release-contract docs, sanitizer coverage for callbacks/progress, and Criterion guard
  commands for the no-callback/no-progress fast paths.

## [0.1.0] — 2026-05-18

First public release of **four** crates. Crate-publish order is load-bearing:
`lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`. The publish is mediated by
[`.github/workflows/release.yml`](.github/workflows/release.yml) — pushing the `v0.1.0` tag
runs the pre-flight gates, the public-API diff, the workspace publish dry-run, and the live
four-crate publish under a single `CARGO_REGISTRY_TOKEN`, then creates the GitHub Release whose
body is this section. See [`docs/release.md`](docs/release.md) for the procedure (CI-mediated
form, with a local fallback for when the workflow is unavailable).

The four-crate shape (rather than three) is the outcome of `RD-2026-05-18-001`: the opinionated
theorem-prover-host stack (`LeanHost` / `LeanCapabilities` / `LeanSession` and their
elaboration / evidence / meta / pool surfaces) split out of `lean-rs` into the new sibling crate
**`lean-rs-host`**. The L1 FFI primitive `lean-rs` now matches the (β)-binding norm — the
OCaml-shaped pattern every mainstream Rust ↔ GC-language binding follows. Downstream consumers
that want only the typed FFI surface depend on `lean-rs` and write their own `@[export]` Lean
shims; consumers that want the curated theorem-prover-host capability stack add `lean-rs-host`.

The supported Lean toolchain window for v0.1.0 is **4.26.0 through 4.29.1** (six releases),
CI-tested in full on `{ubuntu-latest, macos-latest}` per `RD-2026-05-18-002`. The lower bound
was set empirically: a multi-toolchain sweep showed releases ≤ 4.25.x crash inside
`lean_dec_ref_cold` from the L2 host stack — a refcount divergence between 4.25 and 4.26 the
Rust mirrors don't cover. See [`docs/version-matrix.md`](docs/version-matrix.md) and
[`docs/bump-toolchain.md`](docs/bump-toolchain.md) for the bump procedure.

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
- `SUPPORTED_TOOLCHAINS` window table — every Lean release in the supported window with its
  `lean.h` SHA-256 and `missing_symbols` set. `build.rs` accepts any matching digest and emits
  `cargo:rustc-cfg=lean_v_X_Y_Z` for the matched entry; a non-match fails the build with the
  discovered digest and the full window. As of v0.1.0 the window is 4.26.0 through 4.29.1
  (six entries). Lower bound 4.26.0 was set empirically: a multi-toolchain sweep showed
  releases ≤ 4.25.x crash inside `lean_dec_ref_cold` from the L2 host stack — a refcount
  divergence between 4.25 and 4.26 the current mirrors don't cover.
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

Initial release. The L1 FFI primitive — the (β)-binding minimum every embedder needs to call any
`@[export]` Lean function from Rust. Per `RD-2026-05-18-001` this crate no longer ships the
opinionated theorem-prover-host stack (that lives in `lean-rs-host`); `lean-rs` is the typed-FFI
surface plus the four core semantic handle types and the error boundary.

- Single `'lean` lifetime cascade. `LeanRuntime::init` returns a token whose borrow guards every
  downstream handle (`LeanLibrary`, `LeanModule`, `LeanExported`, the four semantic handles).
  Compile-fail tests under `tests/compile_fail/` pin the invariants: handles cannot outlive their
  runtime borrow; runtime and handles are `!Send + !Sync`.
- Curated public surface — three publicly visible modules (`module`, `host`, `error`) plus two
  `pub(crate)` infrastructure modules (`runtime`, `abi`) per `RD-2026-05-17-004`. Boundaries are
  policed by `pub(crate)` rather than crate splits, so they can be reorganized without semver
  breakage. Per-crate baselines under `docs/api-review/`.
- Runtime version probe. After `LeanRuntime::init` succeeds, the runtime cross-checks the active
  `LEAN_VERSION_STRING` against `lean_rs_sys::SUPPORTED_TOOLCHAINS` and returns
  `LeanError::runtime_init_unsupported_toolchain` when the loaded `libleanshared` drifted out
  of the window (e.g., production rolled forward while the binary did not rebuild).
- Typed exported function calls. `LeanExported<'lean, 'lib, Args, R>` per `RD-2026-05-17-007`
  collapsed the prompt-08 arity family into a single generic over a sealed `LeanArgs` trait, an
  `R: DecodeCallResult` return type, and a `LeanAbi` per-type C-ABI representation. `LeanIo<T>`
  expresses the pure-vs-IO return-shape distinction at the type level.
- Two-variant `LeanError` per `RD-2026-05-17-006` — `Lean(LeanException)` for Lean-side exceptions
  and `Host(HostFailure { stage: HostStage })` for host-side setup failures. Structural message
  bounding keeps diagnostics fixed-size. The `LeanDiagnosticCode` projection layered on top
  (`OBSERVABILITY-DIAGNOSTICS`) gives callers a stable caller-facing taxonomy.
- Structured diagnostics — every error-bearing public type projects to `LeanDiagnosticCode` via
  `.code()`. The crate emits `tracing` spans against the `lean_rs` target; in-process tests can
  capture them via `DiagnosticCapture`. No subscriber is installed by the crate; downstream
  consumers attach their own. See `docs/diagnostics.md`.
- `LEAN_RS_NUM_THREADS` environment variable: when set to a positive integer before the first
  `LeanRuntime::init` call, pins the Lean task manager worker count for the lifetime of the
  process. Unset or invalid values fall back to Lean's compiled-in default (typically one worker
  per core) with a `tracing::warn!` for invalid values. Set this when several Lean-using
  processes run side by side to avoid oversubscribing cores.

Known gaps:

- Windows is unsupported (`docs/architecture/02-versioning-and-compatibility.md`).
- The `fuzzing` feature opens a narrow set of `pub(crate)` ABI decoders as `pub fn` entry points
  for the in-tree `fuzz/` crate (cargo-fuzz, nightly-only). It is **not** semver-stable and is
  intentionally invisible in the published docs.

### `lean-rs-host` 0.1.0

Initial release. The L2 opinionated theorem-prover-host stack — the curated session + kernel-check
evidence + bounded `MetaM` + session-pool surface that splits out of `lean-rs` per
`RD-2026-05-18-001`. Consumers add `lean-rs-host = "0.1"` alongside the `lean-rs-host-shims` Lake
package; capability dylibs that load through `LeanCapabilities::load_capabilities` get a full
`LeanSession` cascade.

- `LeanHost` / `LeanCapabilities` / `LeanSession` trio. `LeanHost` is the per-runtime entry point;
  `LeanCapabilities` is the two-dylib loader (consumer dylib + shim dylib, opened in the correct
  order with `RTLD_GLOBAL` so the consumer's transitive references to the shim's
  `initialize_*` symbols resolve); `LeanSession` is the per-call execution context with bulk and
  pool methods.
- Kernel-checked `LeanEvidence` and `ProofSummary` — typed handles for kernel-validated theorems
  with structural diagnostics on elaboration failures (`LeanElabFailure` chain).
- Bounded `MetaM` service registry — `LeanMetaService`, `LeanMetaResponse`, `LeanMetaOptions`,
  and the three pinned constructors `infer_type` / `whnf` / `heartbeat_burn` per
  `RD-2026-05-17-004` Decision 1.
- `SessionPool` / `PooledSession` for fixed-cardinality session reuse; `BATCHING-SESSION-REUSE`
  policy is the initial cut. Cardinality-limit policy and back-pressure shapes may evolve in
  subsequent `0.x` minors.
- 13 mandatory + 3 optional `lean_rs_host_*` `@[export]` Lean shim contract — shipped as the
  in-repo `lake/lean-rs-host-shims` package. Consumers `require` it from
  `git "https://github.com/jcreinhold/lean-rs" @ "v0.1.0" / "lake/lean-rs-host-shims"`; the
  shim package's source is part of this release's tagged commit, not a separate publish.
- Hybrid two-dylib layout — the consumer dylib loads against the shim dylib at runtime via
  `LeanCapabilities`. Both Lake naming conventions (Lean ≤ 4.26 vs Lean ≥ 4.27) are probed by
  the loader; consumers don't have to care which convention their Lake version emits.

Known gaps:

- Windows is unsupported (matches the workspace-wide constraint).
- Same caveats as the `BATCHING-SESSION-REUSE` policy noted above.
- See `docs/lean-rs-host-capability-contract.md` for the full 13+3 shim contract and the
  `LeanDiagnosticCode` taxonomy that surfaces capability-loading failures.

[0.1.1]: https://github.com/jcreinhold/lean-rs/releases/tag/v0.1.1
[0.1.0]: https://github.com/jcreinhold/lean-rs/releases/tag/v0.1.0
