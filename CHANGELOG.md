# Changelog

All notable changes to the five published `lean-rs` workspace crates are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); each crate version is governed by Cargo's `0.x` semver. Items
inside `pub(crate)` modules are not part of the public API and are excluded from this log.

The supported Lean toolchain range, Rust MSRV, and tested platforms for each release are recorded in
[`docs/version-matrix.md`](docs/version-matrix.md); release-time procedure is in [`docs/release.md`](docs/release.md).

## [Unreleased]

## [0.1.7] — 2026-05-24

### `lean-rs-worker` 0.1.7

Two information-loss regressions reported by the first downstream consumer migrating from in-process `LeanSession` to
the worker IPC boundary. Both restore behaviour that the host already implements; the worker now transports it.

- `LeanWorkerKernelResult` now carries `summary: Option<LeanWorkerKernelSummary>`, populated from
  `LeanSession::summarize_evidence` on the `Checked` arm. The `Some iff Checked` invariant is part of the documented
  contract. Restores the proof-summary surface in-process callers relied on before the worker boundary.
- `LeanWorkerSession::infer_type` and `whnf` now attempt notation-aware rendering via the optional `meta_pp_expr` shim
  (`Lean.PrettyPrinter.ppExpr`) and fall back to `Expr.toString` when the shim is absent or reports `Unsupported`. Both
  return `LeanWorkerMetaResult<LeanWorkerRendered>` so callers can see which path was taken via the `rendering`
  (`LeanWorkerRendering::Pretty | Raw`) field. Heartbeat budget is shared with the primary meta call — a slow
  pretty-print on a deeply nested term can in principle starve the meta op (acceptable in practice; `pp_expr` is cheap
  relative to inference).
- A failed `pp_expr` pass (`Failed` / `TimeoutOrHeartbeat`) propagates as the meta call's failure rather than falling
  back to raw. Matches the in-process behaviour the downstream MCP tool already implements.
- Bumped the worker IPC `PROTOCOL_VERSION` from `2` to `3`. The added `summary` field on `LeanWorkerKernelResult` and
  the changed `MetaExpr` payload (`LeanWorkerMetaResult<LeanWorkerRendered>` instead of `<String>`) are
  wire-incompatible with 0.1.6 — a mismatched parent/child pair now fails the handshake with a clear
  `LeanWorkerError::Handshake` rather than a cryptic deserialize error on the first request.

Pre-1.0; the changed return types on `infer_type` / `whnf` and the added field on `LeanWorkerKernelResult` break 0.1.6
callers at the call-site. No other crate in the workspace changes.

### Docs

- Removed the stale post-publish step in `docs/release.md` referencing nonexistent `lean-rs-downstream` and
  `lean-rs-host-downstream` proof repos. No such repositories exist under the project; the line was aspirational.

## [0.1.6] — 2026-05-24

### `lean-rs-worker` 0.1.6

This release adds eight typed methods on `LeanWorkerSession` for the host meta and projection surface, collapses the
worker's public-type layering from three representations per shape to one, streams `list_declarations_strings` so
unbounded Lean environments enumerate without hitting the protocol frame cap, and centralizes the worker's
session-invalidation and IPC round-trip dispatch policies into two private helpers so each typed method delegates to a
single source of truth.

Pre-1.0; the type-surface change is a breaking change against 0.1.5 callers. There are no external consumers of this
crate, so the diff is deliberate.

#### Eight new typed methods on `LeanWorkerSession`

- Added `infer_type`, `whnf`, `is_def_eq`, `describe`, `list_declarations_strings`, `describe_bulk`, `process_file`, and
  `process_module`. Each follows the established `elaborate` / `kernel_check` template — `ensure_open` → typed `Request`
  → typed `Response` projection — and invalidates the session on `Cancelled` / `Timeout`.
- The methods compose existing `LeanSession::*` primitives in the child: `infer_type` / `whnf` run a bounded `MetaM`
  call and render the result `LeanExpr` through `LeanSession::expr_to_string_raw` (deterministic, no second MetaM
  round-trip, same shape `ProcessedFile.TermInfo.expr_str` already uses). `describe` composes three primitives —
  `declaration_kind`, `declaration_type`, `declaration_source_range` — into one IPC round-trip.
- No changes to `lean-rs-sys`, `lean-rs`, `lean-rs-host`, `lean-rs-host-shims`, or the JSON command protocol.

#### Type-surface refactor: one representation per shape

The crate previously held three representations of every value shape: a host type, a `pub(crate)` serde-derived wire
type in `protocol.rs`, and a public `LeanWorker*` mirror in `session.rs` with a hand-written `From` impl. Twenty-one
shapes followed this pattern; for ten of them the public mirror was a pure field-by-field rename with no methods or
validation.

The new layout collapses to one representation:

- New `lean_rs_worker::types` module holds the public, serde-derived `LeanWorker*` types directly. `protocol.rs` keeps
  `Request` / `Response` / framing crate-private and references the public types in its enum variants without an
  intermediate wire layer.
- Conversion from opaque host types (`LeanExpr`, `LeanName`, `LeanElabFailure`, …) into worker value types remains in
  `child.rs` next to the Lean calls that produce them. No `lean_rs_host` type appears in `lean-rs-worker`'s public API.
- Enums in `crate::types` are exhaustive. The worker owns these shapes; their variants are part of the public contract,
  not a host-evolution defence. Generic `LeanWorkerMetaResult<T>` keeps its four variants (`Ok { value }`,
  `Failed { failure }`, `TimeoutOrHeartbeat { failure }`, `Unsupported { failure }`); the variant shape is named-fields
  throughout, matching the wire format.
- Breaking against 0.1.5: pattern matches on `LeanWorkerMetaResult` and `LeanWorkerProcessFileOutcome` now use
  struct-variant syntax (`Ok { value }` / `Processed { file }`) instead of tuple-variant syntax. All other type names
  are preserved.

Net effect: ~21 type definitions and ~21 `From` impls removed (~550 LOC), one named concept per shape, and the worker's
public API decouples from host's internal representation by construction rather than by a translation layer.

#### Streaming `list_declarations_strings`

The public signature is unchanged: `list_declarations_strings(filter, cancellation, progress) -> Result<Vec<String>,
_>`. The implementation now emits one `Message::DataRow` per name from the child and collects them on the parent side,
terminated by `Response::RowsComplete { count }`. The 1 MiB protocol frame cap binds per-row (any individual Lean name
fits well under that) instead of per-response; total environment size is unbounded by framing.

The 0.1.6 work added a doc warning about the per-response cap as a known leak; that warning is gone in this release.
`tests/typed_session.rs` exercises the streaming path against the full Lean stdlib through the fixture environment.

#### Centralized session-invalidation and IPC round-trip policies

Two repeated dispatch policies inside `lean-rs-worker` now live in one place each. The session-invalidation rule
(`Cancelled` / `Timeout` → `LeanWorkerSession::open = false`) is captured in a single private
`LeanWorkerSession::with_session` helper; the 16 typed `LeanWorkerSession` methods (`elaborate`, `kernel_check`,
`infer_type`, `whnf`, `is_def_eq`, `describe`, `list_declarations_strings`, `describe_bulk`, `process_file`,
`process_module`, `run_data_stream`, `run_data_stream_raw`, `capability_metadata`, `capability_doctor`,
`run_json_command`, `run_streaming_command`) each delegate through it instead of inlining the policy. The Worker IPC
round-trip (cancel-check → send → record → read → variant-extract) is captured in a single private
`LeanWorker::round_trip` helper; the 14 simple `worker_*` methods on `LeanWorker` each delegate through it with a small
extract closure, replacing what was a 22-variant exhaustive `Response` wildcard arm per method with a uniform
`unexpected_response` branch. No public API change; no behaviour change.

### Workspace-wide: enums and structs are exhaustive

`#[non_exhaustive]` has been removed from every public enum and struct it was attached to across `lean-rs`,
`lean-rs-host`, `lean-rs-worker`, and `lean-toolchain` — 17 attributes in total (`LeanError`, `HostStage`,
`LeanDiagnosticCode`, `LeanExceptionKind`, `LeanLoaderDiagnosticCode`, `LeanLoaderSeverity`, `EvidenceStatus`,
`LeanKernelOutcome`, `MetaCallStatus`, `LeanMetaResponse`, `LeanMetaTransparency`, `LeanSeverity`, `ProcessFileOutcome`,
`ProcessModuleOutcome`, `LinkDiagnostics`, `LeanModuleDiscoveryDiagnostic`, `ToolchainInfo`, plus the worker's
pre-existing six). Pre-1.0, no external consumers, and host versions are pinned to the workspace, so the forward-compat
insurance these annotations bought wasn't doing meaningful work. Callers that exhaustively matched these enums can now
rely on the variant set being closed; the worker's `child.rs` defensive wildcard arms over host enums
(`LeanMetaResponse`, `LeanKernelOutcome`, `ProcessFileOutcome`, `ProcessModuleOutcome`, `LeanSeverity`) are gone for the
same reason.

## [0.1.5] — 2026-05-23

### `lean-rs-worker` 0.1.5

#### Per-call imports on `LeanWorkerCapability`

- Added `LeanWorkerCapability::open_session_with_imports(imports, cancellation, progress)` alongside the existing
  `open_session`. The new method opens a worker session with a caller-supplied import set, overriding the imports the
  capability was built with; `project_root` / `package` / `lib_name` remain those of the capability. Lifecycle and error
  contract are identical to `open_session`.
- Motivated by parent-side per-imports session caching in downstream MCP hosts that vary `imports` across requests:
  before this method, switching import sets required tearing down the capability (and its worker child). The wire
  protocol already carried `OpenHostSession.imports` per message and the child already opened a fresh `LeanSession` per
  request, so this is a Rust-side ergonomic gap closing, not a new capability.
- Existing `open_session` is unchanged. No removals; additive on the public API.

## [0.1.4] — 2026-05-23

### `lean-rs-host` 0.1.4

#### Header-aware info-tree projection

- Added `LeanSession::process_module_with_info_tree` and the optional shim `lean_rs_host_process_module_with_info_tree`.
  The new entry point parses a file's header with `Lean.Parser.parseHeader` first, then resumes `IO.processCommands`
  from the parser state — so positions in the returned `ProcessedFile` land in the original file's line/column system
  with no Rust-side offset arithmetic. The previous `process_with_info_tree` shim is still the right call for body-only
  snippets and stays untouched.
- Returns a new outcome enum `ProcessModuleOutcome` with four arms — `Ok { file, imports }`,
  `MissingImports { file, imports, missing }`, `HeaderParseFailed { diagnostics }`, and `Unsupported`. Missing imports
  is a soft failure: the body still elaborates against whatever the env carries, and the partial projection is returned
  for downstream consumers to use.
- Capability contract: mandatory count unchanged (28); optional count 6 → 7. Capability dylibs built against the 0.1.3
  shim set continue to load — the new symbol degrades to `ProcessModuleOutcome::Unsupported`.

### `lean-rs` 0.1.4

#### Runtime initialization

- `LeanRuntime::init` now calls `lean_io_mark_end_initialization()` after the core-runtime + built-in bootstrap. Without
  this call, Lean's `IO.initializing` flag stayed `true` for the process lifetime and any Lean API gated on it (most
  notably `Lean.mkEmptyEnvironment`, transitively used by `Lean.Parser.parseHeader`) threw
  `"environment objects cannot be created during initialization"`. The omission was documented as already-fixed in
  `crates/lean-rs/examples/cold_probe.rs:62` but was missing from the runtime init body. No public API change;
  downstream module initializers loaded via `LeanLibrary::initialize_module` continue to run normally because
  Lake-emitted initializers do not check the flag.

## [0.1.3] — 2026-05-23

Adds string projections for opaque `LeanName` and `LeanExpr` handles and a new info-tree projection over processed Lean
sources. Concentrated in `lean-rs-host`; the other four crates have only the toolchain-window extension. The supported
Lean window now covers **4.26.0 through 4.29.1 plus the 4.30.0-rc2 release candidate**.

### `lean-rs-host` 0.1.3

#### `LeanName` rendering

- Added `LeanSession::name_to_string`, `name_to_string_bulk`, and `list_declarations_strings` for projecting opaque
  `LeanName` handles into Rust `String`s. Backed by the new mandatory shim `lean_rs_host_name_to_string`. The handle
  stays opaque — no `Display`, `Eq`, or `From<String>` — so the FFI cost is visible at the call site and the
  diagnostic-only semantics are not papered over.

#### `LeanExpr` rendering

- Added two complementary projections so callers pick the cost tier without a flag.
  - `LeanSession::expr_to_string_raw` walks `Expr.toString` through the new mandatory shim
    `lean_rs_host_env_expr_to_string_raw`. No `MetaM`, no notation, ugly but deterministic — suitable for indexing,
    logging, and search keys.
  - `LeanSession::pp_expr` runs `Lean.PrettyPrinter.ppExpr` as a new optional meta service, heartbeat-bounded by
    `LeanMetaOptions`. Capability dylibs that predate the service still load; `run_meta` returns `Unsupported` so
    callers can fall back to the raw path.

#### Info-tree projection

- Added `LeanSession::process_with_info_tree`. The session projects a Lean source into a `ProcessedFile`: command, term,
  and tactic nodes plus name references, each carrying source ranges and diagnostics. The method returns
  `ProcessFileOutcome::Processed` or `ProcessFileOutcome::Unsupported`, so capability dylibs without the new optional
  shim `lean_rs_host_process_with_info_tree` still load.

#### Capability contract

- Mandatory shim count: 26 → 28. Optional shim count: 3 → 6. Capability dylibs built against the 0.1.2 shim set **must
  be rebuilt** before they will pass 0.1.3 capability preflight — the two new mandatory entries
  (`lean_rs_host_name_to_string`, `lean_rs_host_env_expr_to_string_raw`) have no fallback path. The new optional entries
  degrade cleanly: missing `pp_expr` and `process_with_info_tree` surface as `Unsupported` rather than load failure.

### Lean toolchain window

- Added 4.30.0-rc2 to `SUPPORTED_TOOLCHAINS` (header digest
  `790b121ce52942086a360a91f6db5f0f738043bc87b669daffa3fb8bc01e6dd3`). Layout-probe + symbol-probe gates both clean
  against 4.29.1. RCs are now in scope as supported rows; promotion to the stable row happens when upstream tags
  `4.30.0`.
- Fixed `lean-rs-sys`'s build script: the `lean_v_X_Y_Z` cfg-token converter now sanitizes any non-identifier character
  (hyphens in `-rcN` suffixes specifically), preventing `invalid --cfg argument` build failures on RC toolchains.

### CI

- Bumped the CI / sanitizer / release head pin from 4.29.1 to 4.30.0-rc2 and extended the workflow-dispatch full matrix
  to cover every entry in the table.
- Narrowed the sanitizer job to Linux ASan Rust-only; the previous matrix is documented in
  [`docs/safety/unsafe-inventory.md`](docs/safety/unsafe-inventory.md).
- Gated the fuzz workflow inside the release pipeline so tag pushes run it without making it a default-branch gate.

### Internal

- Repointed `scripts/test-all-toolchains.sh` to the bundled shim packages under `crates/lean-rs/shims/` and
  `crates/lean-rs-host/shims/` (was top-level `lake/`), added the `templates/shipped-lean-crate/lean/` pin, and brought
  the script up to shellcheck/shfmt clean (real bug fix on a `[ ... =~ ... ]` test).

## [0.1.2] — 2026-05-21

### Shipping Lean code

- Added the canonical build-time shipping path for downstream crates with `lean_toolchain::CargoLeanCapability`,
  `lean_rs::LeanCapability`, `lean_rs_worker::LeanWorkerChild`, and the `ship-crate-with-lean` recipe/template.

### Documentation builds

- Fixed docs.rs builds by making `lean-rs-sys` emit documentation-only toolchain metadata when `DOCS_RS=1`, instead of
  probing for a Lean installation that docs.rs does not provide.
- Added explicit docs.rs metadata for each public crate so docs.rs builds only the default Linux target instead of
  relying on service defaults.
- Added a `DOCS_RS=1` workspace documentation gate to CI and the release workflow, plus a packaged-tarball docs.rs
  simulation that hides Lean/elan/lake from `PATH` before building docs from normalized crate contents.
- Included `lean-rs-worker` benchmark sources in the crate package so declared bench targets do not produce packaging
  warnings.

### Loader and deployment hardening

- Added the manifest-backed `LeanCapability` bundle-loader path, loader preflight diagnostics, cross-platform loader
  regressions, and worker bootstrap checks as the patch-release contract for shipped Lean capabilities.
- Made the intended hierarchy explicit in docs and examples: `CargoLeanCapability` manifest → `LeanCapability` bundle
  loader → optional `LeanWorkerCapabilityBuilder` / `LeanWorkerPool`. Lower-level `LeanLibrary` calls, raw link helpers,
  and low-level worker APIs remain advanced escape hatches.
- Added CI/release gates for packaged-tarball docs.rs simulation, loader regressions, workflow validation, package
  creation, and public-API baseline drift.

## [0.1.1] — 2026-05-20

Hardening release for the Lean/Rust interop stack and the first publish of **`lean-rs-worker`**, the worker-process
boundary around `lean-rs-host`. After this release crates.io has all five workspace crates at 0.1.1. The Lean toolchain
window stays at **4.26.0 through 4.29.1**.

### `lean-rs-sys` 0.1.1

- Added the internal `metadata-only` feature so `lean-toolchain` can depend on build-time Lean metadata without linking
  downstream `build.rs` binaries to `libleanshared`.

### `lean-toolchain` 0.1.1

- Added `build_lake_target(project_root, target_name)` and `build_lake_target_quiet(project_root, target_name)` for Lake
  shared-library targets. The helpers hide Lake output naming, cache hits, cache misses, and Cargo rerun directives
  behind typed `LinkDiagnostics`.
- Added `emit_lean_link_directives_checked()` for callers that want typed link diagnostics rather than warning-only
  output.

### `lean-rs` 0.1.1

- Added the L1 callback registry: `LeanCallbackHandle<P: LeanCallbackPayload>`, `LeanProgressTick`, `LeanStringEvent`,
  `LeanCallbackFlow`, and `LeanCallbackStatus`. Lean can call Rust through opaque handles and crate-owned trampolines
  without exposing public raw callback pointers. Callback payloads are a sealed family; downstream crates can use the
  supported tick and string payloads but cannot add arbitrary payload ABI shapes.
- Bundled the generic `lean-rs-interop-shims` Lake package under the crate so downstream L1 consumers do not depend on
  in-tree development paths.
- Added the downstream interop example and recipe covering Rust-to-Lean exported calls plus Lean-to-Rust callbacks
  without `lean-rs-host`.
- Added the string streaming callback example and recipe, showing Lean-to-Rust JSONL-like row streaming through
  `LeanCallbackHandle<LeanStringEvent>` without making `lean-rs` own the row schema.

### `lean-rs-host` 0.1.1

- Added cooperative cancellation (`LeanCancellationToken`) and structured progress (`LeanProgressSink`,
  `LeanProgressEvent`) to long-running host-session operations.
- Added source-range lookup, filtered declaration listing, `is_def_eq`, and the three bulk declaration-property methods.
- Bundled the host and generic shim packages under the crate. Consumers no longer add `lean_rs_host_shims` or
  `lean_rs_interop_shims` to their own `lakefile.lean`; the host loader builds and opens the bundled shims on demand.
- Added release-contract docs, sanitizer coverage for callbacks/progress, and Criterion guard commands for the
  no-callback/no-progress fast paths.

### `lean-rs-worker` 0.1.1

- Added the worker-process boundary around `lean-rs-host`: `LeanWorker`, `LeanWorkerConfig`, typed child-exit errors,
  explicit restart, clean shutdown, and worker statistics. The worker hides process spawning, pipes, frame decoding,
  fatal-child-exit classification, and cleanup from callers.
- Added process-cycling policy for Lean process-global memory retention: explicit cycling, max-request and
  max-import-like thresholds, idle cycling, best-effort RSS ceilings, and restart reasons that distinguish policy
  cycling from child crashes and request timeouts.
- Added the narrow worker session adapter for copied host-session results, then the generic worker capability layer:
  live bounded row forwarding, `LeanWorkerDataRow`, `LeanWorkerDataSink`, diagnostics, terminal summaries, request
  watchdogs, capability metadata, doctor checks, `LeanWorkerCapabilityBuilder`, typed JSON commands, and typed streaming
  commands.
- Added worker examples and recipes for process-boundary use, memory cycling, arbitrary downstream-owned rows,
  capability startup, typed commands, timeouts, and performance probes.
- Added the production-scale worker contract docs for the local `LeanWorkerPool` foundation: planner → pool → session
  lease → typed command → live rows → terminal summary → pool stats. The contract records remote workers, byte
  callbacks, object callbacks, and downstream schemas as non-goals for the current scale release.

## [0.1.0] — 2026-05-18

First public release of **four** crates. Crate-publish order is load-bearing: `lean-rs-sys` → `lean-toolchain` →
`lean-rs` → `lean-rs-host`. The publish is mediated by [`.github/workflows/release.yml`](.github/workflows/release.yml):
pushing the `v0.1.0` tag runs the pre-flight gates, the public-API diff, the workspace publish dry-run, and the live
four-crate publish under a single `CARGO_REGISTRY_TOKEN`, then creates the GitHub Release whose body is this section.
See [`docs/release.md`](docs/release.md) for the procedure (CI-mediated form, with a local fallback for when the
workflow is unavailable).

The opinionated theorem-prover-host stack (`LeanHost` / `LeanCapabilities` / `LeanSession` and their elaboration /
evidence / meta / pool surfaces) ships as the sibling crate **`lean-rs-host`** rather than living inside `lean-rs`. L1
consumers that just want the typed FFI surface depend on `lean-rs` and write their own `@[export]` Lean shims; consumers
that want the curated theorem-prover-host capability stack add `lean-rs-host`. This matches the standard two-crate shape
of mainstream Rust bindings (a raw `*-sys` plus a safe front door).

The supported Lean toolchain window for v0.1.0 is **4.26.0 through 4.29.1** (six releases), CI-tested in full on
`{ubuntu-latest, macos-latest}`. The lower bound was set empirically: a multi-toolchain sweep showed releases ≤ 4.25.x
crash inside `lean_dec_ref_cold` from the L2 host stack: a refcount divergence between 4.25 and 4.26 the Rust mirrors
don't cover. See [`docs/version-matrix.md`](docs/version-matrix.md) and
[`docs/bump-toolchain.md`](docs/bump-toolchain.md) for the bump procedure.

### `lean-rs-sys` 0.1.0

Initial release. Raw FFI bindings for the Lean 4 C ABI.

- Opaque public types. `lean_object` is `[u8; 0]` plus phantom markers (`!Send + !Sync + !Unpin`); the layout struct
  `LeanObjectRepr` is `pub(crate)`. Downstream code reaches refcount, tag, and payload state only through this crate's
  `pub unsafe fn` helpers; reading struct fields is not available.
- `# Safety` discipline. Every `pub unsafe fn` (99 of them across `array`, `closure`, `ctor`, `external`, `io`,
  `object`, `refcount`, `scalar`, `string`) carries a `# Safety` section naming the precondition. Every `unsafe { ... }`
  block carries a `// SAFETY:` comment restating the specific invariant. The lint at
  `crates/lean-rs-sys/tests/safety_grep.rs` enforces presence.
- Pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, alongside the extern declarations for the matching
  category (refcount, string, array, …).
- `REQUIRED_SYMBOLS` allowlist: 87 `LEAN_EXPORT`'d symbol names this crate's `extern "C"` blocks declare;
  `tests/linkage.rs` resolves every entry against `libleanshared` at link time.
- `SUPPORTED_TOOLCHAINS` window table: every Lean release in the supported window with its `lean.h` SHA-256 and
  `missing_symbols` set. `build.rs` accepts any matching digest and emits `cargo:rustc-cfg=lean_v_X_Y_Z` for the matched
  entry; a non-match fails the build with the discovered digest and the full window. As of v0.1.0 the window is 4.26.0
  through 4.29.1 (six entries). Lower bound 4.26.0 was set empirically: a multi-toolchain sweep showed releases ≤ 4.25.x
  crash inside `lean_dec_ref_cold` from the L2 host stack: a refcount divergence between 4.25 and 4.26 the current
  mirrors don't cover.
- Features: `dynamic` + `mimalloc` (default), `static` (opt-in; selecting it requires extending the link set beyond what
  `lean.h` alone demands; see `build.rs`).

Known gaps:

- Lean's header layout (`LeanObjectRepr` field order) is intentionally **not** part of this crate's public semver.
  Layout-tracking updates to track a new Lean point release are minor bumps; the opaque public types are unchanged.
- Windows is unsupported (`docs/architecture/02-versioning-and-compatibility.md`).

### `lean-toolchain` 0.1.0

Initial release. Lean 4 toolchain discovery, fingerprint, fixture digest, link diagnostics, and reusable `build.rs`
helpers for downstream embedders.

- Typed `ToolchainFingerprint` carrying the active Lean version, header path, and header digest, plus the
  `LAKE_FIXTURE_DIGEST` constant covering the in-tree fixture package.
- Pass-through re-exports for `LEAN_HEADER_DIGEST`, `LEAN_HEADER_PATH`, `LEAN_VERSION` from `lean-rs-sys`, so a
  downstream `build.rs` can read the toolchain pins without taking a direct `lean-rs-sys` dependency.
- `required_symbols()` returns `lean_rs_sys::REQUIRED_SYMBOLS` directly, keeping the allowlist in one place.
- Layered link diagnostics that locate the active `libleanshared` and emit bounded error messages when a required symbol
  is missing or the header digest does not match.

### `lean-rs` 0.1.0

Initial release. The L1 FFI primitive: the minimum every embedder needs to call any `@[export]` Lean function from Rust.
`lean-rs` is the typed-FFI surface plus the four core semantic handle types and the error boundary; the opinionated
theorem-prover-host stack lives in the sibling `lean-rs-host` crate.

- Single `'lean` lifetime cascade. `LeanRuntime::init` returns a token whose borrow guards every downstream handle
  (`LeanLibrary`, `LeanModule`, `LeanExported`, the four semantic handles). Compile-fail tests under
  `tests/compile_fail/` pin the invariants: handles cannot outlive their runtime borrow; runtime and handles are
  `!Send + !Sync`.
- Curated public surface: three publicly visible modules (`module`, `host`, `error`) plus two `pub(crate)`
  infrastructure modules (`runtime`, `abi`). Boundaries are policed by `pub(crate)` rather than crate splits, so they
  can be reorganized without semver breakage. Per-crate baselines under `docs/api-review/`.
- Runtime version probe. After `LeanRuntime::init` succeeds, the runtime cross-checks the active `LEAN_VERSION_STRING`
  against `lean_rs_sys::SUPPORTED_TOOLCHAINS` and returns `LeanError::runtime_init_unsupported_toolchain` when the
  loaded `libleanshared` drifted out of the window (e.g., production rolled forward while the binary did not rebuild).
- Typed exported function calls. `LeanExported<'lean, 'lib, Args, R>` is one generic over a sealed `LeanArgs` trait, an
  `R: DecodeCallResult` return type, and a `LeanAbi` per-type C-ABI representation. `LeanIo<T>` expresses the pure-vs-IO
  return-shape distinction at the type level.
- Two-variant `LeanError`: `Lean(LeanException)` for Lean-side exceptions and `Host(HostFailure { stage: HostStage })`
  for host-side setup failures. Structural message bounding keeps diagnostics fixed-size. A `LeanDiagnosticCode`
  projection layered on top gives callers a stable caller-facing taxonomy.
- Structured diagnostics: every error-bearing public type projects to `LeanDiagnosticCode` via `.code()`. The crate
  emits `tracing` spans against the `lean_rs` target; in-process tests can capture them via `DiagnosticCapture`. No
  subscriber is installed by the crate; downstream consumers attach their own. See `docs/diagnostics.md`.
- `LEAN_RS_NUM_THREADS` environment variable: when set to a positive integer before the first `LeanRuntime::init` call,
  pins the Lean task manager worker count for the lifetime of the process. Unset or invalid values fall back to Lean's
  compiled-in default (typically one worker per core) with a `tracing::warn!` for invalid values. Set this when several
  Lean-using processes run side by side to avoid oversubscribing cores.

Known gaps:

- Windows is unsupported (`docs/architecture/02-versioning-and-compatibility.md`).
- The `fuzzing` feature opens a narrow set of `pub(crate)` ABI decoders as `pub fn` entry points for the in-tree `fuzz/`
  crate (cargo-fuzz, nightly-only). It is **not** semver-stable and is intentionally invisible in the published docs.

### `lean-rs-host` 0.1.0

Initial release. The L2 theorem-prover-host stack: the curated session + kernel-check evidence + bounded `MetaM` +
session-pool surface built on top of `lean-rs`. Consumers add `lean-rs-host = "0.1"` alongside the `lean-rs-host-shims`
Lake package; capability dylibs that load through `LeanCapabilities::load_capabilities` get a full `LeanSession`
cascade.

- `LeanHost` / `LeanCapabilities` / `LeanSession` trio. `LeanHost` is the per-runtime entry point; `LeanCapabilities` is
  the two-dylib loader (consumer dylib + shim dylib, opened in the correct order with `RTLD_GLOBAL` so the consumer's
  transitive references to the shim's `initialize_*` symbols resolve); `LeanSession` is the per-call execution context
  with bulk and pool methods.
- Kernel-checked `LeanEvidence` and `ProofSummary`: typed handles for kernel-validated theorems with structural
  diagnostics on elaboration failures (`LeanElabFailure` chain).
- Bounded `MetaM` service registry: `LeanMetaService`, `LeanMetaResponse`, `LeanMetaOptions`, and the three pinned
  constructors `infer_type` / `whnf` / `heartbeat_burn`.
- `SessionPool` / `PooledSession` for fixed-cardinality session reuse; `BATCHING-SESSION-REUSE` policy is the initial
  cut. Cardinality-limit policy and back-pressure shapes may evolve in subsequent `0.x` minors.
- 13 mandatory + 3 optional `lean_rs_host_*` `@[export]` Lean shim contract: shipped as the in-repo
  `lake/lean-rs-host-shims` package. Consumers `require` it from
  `git "https://github.com/jcreinhold/lean-rs" @ "v0.1.0" / "lake/lean-rs-host-shims"`; the shim package's source is
  part of this release's tagged commit, not a separate publish.
- Hybrid two-dylib layout: the consumer dylib loads against the shim dylib at runtime via `LeanCapabilities`. Both Lake
  naming conventions (Lean ≤ 4.26 vs Lean ≥ 4.27) are probed by the loader; consumers don't have to care which
  convention their Lake version emits.

Known gaps:

- Windows is unsupported (matches the workspace-wide constraint).
- Same caveats as the `BATCHING-SESSION-REUSE` policy noted above.
- See `docs/lean-rs-host-capability-contract.md` for the full 13+3 shim contract and the `LeanDiagnosticCode` taxonomy
  that surfaces capability-loading failures.

[0.1.2]: https://github.com/jcreinhold/lean-rs/releases/tag/v0.1.2
[0.1.1]: https://github.com/jcreinhold/lean-rs/releases/tag/v0.1.1
[0.1.0]: https://github.com/jcreinhold/lean-rs/releases/tag/v0.1.0
