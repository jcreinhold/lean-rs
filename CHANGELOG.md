# Changelog

All notable changes to the published `lean-rs` workspace crates are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); each crate version is governed by Cargo's `0.x` semver. Items
inside `pub(crate)` modules are not part of the public API and are excluded from this log.

The supported Lean toolchain range, Rust MSRV, and tested platforms for each release are recorded in
[`docs/version-matrix.md`](docs/version-matrix.md); release-time procedure is in [`docs/release.md`](docs/release.md).

## [Unreleased]

### `lean-rs-abi` is purely static; toolchain probing moves to the crates that link or discover Lean

`lean-rs-abi` no longer has a build script. Previously its `build.rs` probed an installed Lean toolchain (hashing
`lean.h`, reading the version) and panicked when none was found, so a crate documented as "link-free metadata" could not
be built by any pure-Rust consumer without a Lean toolchain on `PATH` (or `DOCS_RS=1`) — which kept downstream Rust-only
CI red. The crate is now purely static: the `SUPPORTED_TOOLCHAINS` window, `REQUIRED_SYMBOLS`, and the `lean.h` layout
literals, with no probe.

The live toolchain identity it used to bake (`LEAN_VERSION`, `LEAN_RESOLVED_VERSION`, `LEAN_HEADER_PATH`,
`LEAN_HEADER_DIGEST`) moves to the crates that legitimately have a toolchain at build time:

- `lean-rs-sys` (which links `libleanshared`) now bakes these from its own build script and exposes them at the same
  paths (`lean_rs_sys::LEAN_VERSION`, `lean_rs_sys::consts::*`).
- `lean-toolchain` (the discovery crate) now bakes them in its own `build.rs` and re-exports them at the same paths
  (`lean_toolchain::LEAN_VERSION`, …). Its probe **degrades** to the latest supported window entry when no toolchain is
  present instead of failing, so link-free downstream crates (and `docs.rs`) build with no Lean installed. It still
  fails the build when an *installed* toolchain's `lean.h` is outside the supported window.

`lean_rs_abi::LEAN_VERSION`, `LEAN_RESOLVED_VERSION`, `LEAN_HEADER_PATH`, and `LEAN_HEADER_DIGEST` are removed; consume
them from `lean-rs-sys` or `lean-toolchain` instead. No other public API or layout change.

## [0.2.2] - 2026-06-10

### Supported Lean toolchain window: add 4.31.0-rc2

Lean 4.31.0-rc2 ships a byte-identical `lean.h` to 4.31.0-rc1, so it joins the same ABI-equivalence entry in
`SUPPORTED_TOOLCHAINS` and becomes the new head of the supported window (now 4.26.0 through 4.31.0-rc2). No public API or
layout change; the workspace test suite passes against rc2 on the local toolchain sweep.

### Release recovery covers all publishable crates

The `release-recover.yml` recovery workflow now walks every publishable crate in dependency order, so a partial publish
that stalls on any crate—not just the tail—can be completed without burning a version.

## [0.2.1] - 2026-06-09

### `lean-toolchain`: explicit capability manifest dependencies

`CargoLeanCapability` can now record caller-provided `LeanLibraryDependency` entries in the capability artifact
manifest. Downstream build scripts that build a primary Lake shared library against another packaged Lean shared library
can describe that dependency before the manifest is written, instead of editing JSON after `build_quiet()` returns. This
is domain-neutral: source ownership, package names, module names, and export metadata still live in the downstream
package that owns the Lean payload.

### `lean-toolchain`: shared source-package materialization

`lean-toolchain` now exposes a domain-neutral source-package materializer for crates that ship Lean source payloads and
need to build them under a caller-selected toolchain. The helper owns the repeated cache mechanics: digest-keyed cache
entries, per-entry file locks, atomic temp-directory installation, generated `lean-toolchain` files, provenance
sidecars, generated-file validation, and explicit Lake manifest policy. It hides the copy/cache protocol behind one
request type so downstream crates do not hand-roll sibling-checkout discovery or racy `.lake` reuse.

### `lean-rs-interop-shims`: release-shaped package surface

The package-owned `lean-rs-interop-shims` crate now uses the shared materializer, ships its license texts explicitly,
and has a package-list regression proving the Lean/C/Lake runtime payload and licenses are present while Rust crate
scaffolding is not copied into materialized Lake roots. `lean-rs-host`'s parity guard now compares the runtime payload
only, so the canonical package can carry Rust packaging files without forcing the host's bundled Lean payload copy to
mirror them.

## [0.2.0]

This is a breaking release (pre-1.0, so a minor bump). The worker wire protocol moved from `PROTOCOL_VERSION` 8 to 10,
several `LeanWorkerError` variants gained fields and boxed an existing one, and `Request::OpenHostSession` /
`Response::HostSessionOpened` gained fields — a parent and child built from different `0.x` lines will refuse to
handshake. Rebuild the whole worker stack from one version. `lean-toolchain`'s `DiscoverOptions` also gains a field
(`allow_lean_sysroot_env`, see below). The headline theme is bounded, observable imports: every
host session now reports what it loaded and how much memory that cost, the pool can refuse imports that would breach an
RSS budget, and resource exhaustion surfaces as structured facts instead of an opaque failure.

### New crate: `lean-rs-abi` (link-free Lean ABI and toolchain metadata)

The link-free Lean 4 ABI constants and toolchain-fingerprint metadata that previously lived in `lean-rs-sys` now live in
a new published crate, `lean-rs-abi`, which carries no link dependency on `liblean`. `lean-rs-sys` glob-re-exports
`lean_rs_abi::consts::*` and `lean_rs_abi::supported::*`, so existing paths such as `lean_rs_sys::consts::LEAN_ARRAY`,
`lean_rs_sys::SUPPORTED_TOOLCHAINS`, and `lean_rs_sys::SupportedToolchain` keep resolving unchanged; the items are simply
defined one crate deeper. Build tooling that needs the digest window or ABI tags without linking Lean can now depend on
`lean-rs-abi` directly. (`cargo public-api` cannot enumerate a cross-crate glob, so the `lean-rs-sys` baseline now renders
these as `pub use …::<<lean_rs_abi::…::*>>` rather than item-by-item; the public surface is unchanged.)

### `lean-toolchain`: an explicit sysroot no longer leaks ambient `LEAN_SYSROOT`

`DiscoverOptions` gains an `allow_lean_sysroot_env` flag, completing the per-probe gating set. Previously the
`LEAN_SYSROOT` environment probe ran unconditionally, so passing an explicit (or deliberately invalid) sysroot still
fell back to an ambient `LEAN_SYSROOT` — contradicting `explicit_sysroot`'s documented "probed first, ambient disabled"
contract. The build helpers now clear this flag whenever an explicit sysroot is set, so an explicit sysroot is the only
source. `Default` leaves it enabled, preserving the previous discovery order for callers that do not narrow the probe
set. This is the field that surfaced as a CI-only failure (CI exports `LEAN_SYSROOT`); local runs without it were
unaffected.

### `lean-rs-host`: session import profiles bound what a session loads

New `LeanSessionImportProfile` (`Private`, `Server`, `ExportedPublic`, `FullPrivateCompat`) and the lower-level
`LeanImportLevel` / `LeanImportProfileMode` let a caller pick how much of a module's import closure a session pulls in,
instead of always importing everything. `LeanCapabilities::session_with_profile(...)` and
`SessionPool::acquire_with_profile(...)` open a session under a chosen profile; the existing `session(...)` /
`acquire(...)` keep the full-import behavior. On the wire this adds `import_profile` to `Request::OpenHostSession`
(part of the `PROTOCOL_VERSION` bump). Profiles default to the full-compatibility behavior, so an unset profile imports
as before.

### `lean-rs-host`: import statistics and compact-region memory attribution

Every opened session now records a `LeanImportStats` (`LeanSession::import_stats()`), reporting effective module count,
imported byte and constant counts, extension counts, and per-region memory attribution split into memory-mapped,
non-memory-mapped, and compacted-region bytes/counts. `LeanImportStats::memory_diagnostic()` renders a human-readable
summary. `LeanCapabilities::profiling_session(...)` plus `LeanImportProfilerOptions` (`profiler`, `trace_profiler`,
`trace_profiler_output`) opt into Lean's import profiler for a one-off measured open. The stats ride the protocol as
`import_stats` on `Response::HostSessionOpened` and on the bracketed-import result (below), and are mirrored by
`lean-rs-worker-protocol`'s `LeanWorkerImportStats`.

### `lean-rs-host`: session-pool memory policy bounds import admission

`SessionPoolMemoryPolicy` lets a pool refuse work that would breach a memory ceiling: `max_rss_kib(...)` caps process
RSS and `max_fresh_imports(...)` caps how many fresh import-bearing sessions may be admitted, with `disabled()` for the
previous unbounded behavior. `SessionPoolConfig` (and `SessionPool::with_config` / `with_memory_policy`) thread the
policy in, and `SessionPool::memory_policy()` reads it back. When the policy refuses admission the caller gets a
resource-exhausted error (below) rather than an OOM abort.

### Resource exhaustion is now a structured, honest diagnostic

Memory- and admission-pressure failures surface as facts instead of an opaque error. `lean-rs` gains
`ResourceExhaustedFacts` (cause, current/limit RSS, import counts and limits, whether work entered Lean, last import
stats), reachable via `HostFailure::resource_exhausted_facts()`, plus a `HostStage::Resource` stage and a
`LeanDiagnosticCode::ResourceExhausted` code. `lean-rs-worker-protocol` carries the richer `LeanWorkerResourceExhaustedFacts`
(adding operation, queue-wait and duration timing, worker generation, cold-open and import-like admission counters, and
restart reason). **Breaking:** several `LeanWorkerError` variants (`Timeout`, `Cancelled`, `RssHardLimitExceeded`,
`WorkerPoolExhausted`, `WorkerPoolMemoryBudgetExceeded`, `WorkerPoolQueueTimeout`) gained a boxed `resource` field, and
the existing `last_import_stats` field on the RSS/budget variants is now boxed (`Option<Box<LeanWorkerImportStats>>`);
read them through the new `LeanWorkerError::resource_exhausted_facts()` accessor.

### `lean-rs-worker-parent`: session-reuse key diagnostics

`LeanWorkerPoolSnapshot` gains counters that make warm-session reuse auditable: `key_hits`, `key_misses`,
`distinct_keys_seen`, `last_key_miss_reason`, `import_like_requests`, and the cold-open accounting
(`cold_open_attempts`, `cold_open_admitted`, `cold_open_refusals`, `concurrent_cold_opens_observed`,
`fresh_cold_opens_avoided`). The host side exposes the same classification as `SessionPoolKeyMissReason`. These make it
possible to tell whether a workload is fragmenting the session-reuse key and paying for avoidable cold opens.

### `lean-rs-worker-parent`: worker-replacement latency is measured

`LeanWorkerReplacementTiming` plus the `last_replacement_timing`, `last_capability_load_elapsed`,
`last_first_command_elapsed`, and `last_replacement_skipped_reason` fields on the pool snapshot record how long it takes
to stand up a replacement child (capability dylib load and first command) so replacement cost shows up in diagnostics
instead of as unexplained latency.

### Bracketed lightweight import queries and batched warm module queries

The host gains a bracketed import path (`LeanBracketedImportResult`, carrying its own `import_stats`) for lightweight
module queries that do not need a full session, and the worker exposes
`LeanWorkerSessionLease::process_module_query_batch(...)` (`LeanWorkerModuleQuerySelector` →
`LeanWorkerModuleQueryBatchOutcome`) so a warm child can answer a batch of module queries in one round trip instead of
one command each.

### Derived-work observability (`LeanWorkerDerivedWorkFacts`)

Declaration inspection and proof-search results now attach `LeanWorkerDerivedWorkFacts`, counting the derived work a
query triggered: parser/elaborator runs, pretty-prints, raw type renderings, docstring lookups, source-range lookups,
simp-extension lookups, module-snapshot builds, proof-search fact collections, and whether lazy discrimination-tree
import initialization was observed. Proof-search facts are gated by a new `proof_search` inspection field and report
`computed` / `unavailable_reason`, so a caller can tell eager from skipped derived work and avoid paying for it
needlessly.

### `lean-rs-worker-parent`: capability sessions can import an external Lake workspace

`LeanWorkerCapabilityBuilder::import_workspace_root(...)` lets a manifest-backed worker capability load its dylib from
the capability project while opening the host session against a separate audited Lake workspace. The import root is one
Lake project and its manifest-declared dependency closure; it is not merged with the capability project's search path,
so capability dependencies cannot shadow the audited workspace's dependencies. The unset path keeps the previous
behavior (`project_root` is also the import root). Consumers whose capability project and target workspace differ must
set this override explicitly.

Capability exports that import modules must rely on the host-installed search path. They must not call
`Lean.initSearchPath` or reconstruct the search path from `LEAN_PATH`, because that would reset Lean's search path and
discard the selected import workspace root.

The worker-pool session key now includes the canonical effective import workspace root, so repeated audits of the same
workspace reuse the warm child session while different audited workspaces do not alias. The integration fixture records
the external-root workload, platform, module counts, cold first-lease cost, warm second-lease cost, worker count, warm
lease count, and import counter; warm same-workspace commands attach to the already-open child session instead of
reopening imports. This is parent-side only: `OpenHostSession.project_root` already existed on the wire, so
`lean-rs-worker-protocol`, `lean-rs-worker-child`, `lean-rs-host`, the Lean shims, and `PROTOCOL_VERSION` are unchanged.
The pool intentionally keeps one worker entry per compatible capability/import-workspace session for now; reusing one
loaded capability child across many target workspaces is deferred until a named workload measures dylib reload or worker
churn as the bottleneck.

## [0.1.20]

### Pristine-entry proof position (`ProofPositionSelector::Entry`)

- **New `Entry` proof-position selector — the goal state before any tactic runs.** `proof_state` already exposed the
  pristine entry goal as the `goals_before` of the first position, but `try_proof_step` could only splice a candidate
  *after* the first tactic, so a from-scratch tactic block (e.g. `intro …; exact …`) read from `goals_before` ran with
  the first binder already introduced and failed. `Entry` anchors on the first tactic's pre-state and splices the
  candidate *before* the first tactic (aligned to its column, never dedented to a top-level command), so a from-scratch
  block elaborates against the untouched goal and the original tactics' downstream errors are reported as downstream,
  not candidate-local. For `proof_state`, `Entry` renders `goals_before == goals_after`. The variant is additive on the
  `#[non_exhaustive]` `LeanWorkerProofPositionSelector` (wire) and `ProofPositionSelector` (host); `Index { index }`
  keeps its existing "index-th tactic state" meaning. See [`docs/architecture/09-info-tree-projection.md`](docs/architecture/09-info-tree-projection.md).

### Worker robustness: no verify crash, honest degraded verdict, pretty proof-state locals

A heavy `verify_declaration` / `proof_state` workload run against a RED module while the worker thrashed against its RSS
cap surfaced three defects in the read-only query path; all three are fixed.

- **`verify_declaration` no longer aborts the child on a proof state degraded by memory pressure.** The
  `Lean.MetavarContext.getDecl … unknown metavariable` panic on the proof-state walk is an uncatchable `abort()` under
  the child's `LEAN_ABORT_ON_PANIC=1`, so it is contained at two layers. The shim screens a captured proof state for
  dangling metavariables with the *total* `MetavarContext.findDecl?` (never the pure-`panic!` `getDecl`) before any
  renderer dereferences it, rendering a degraded goal as `<goal unavailable: elaboration degraded under resource
  pressure>` and routing the verdict to `BudgetExceeded` with `axioms_available = false`. As an authoritative backstop
  for any residual transitive-mvar abort, the supervisor maps a `ChildPanicOrAbort` during a verify / query-batch
  request to a synthesized degraded verdict (verify → `BudgetExceeded` with unavailable facts; batch → per-selector
  `BudgetExceeded`) and records a `child_abort` restart, so the caller always gets a verdict and the next call is served
  by a fresh child.
- **`verify_declaration` no longer returns `NotFound` for a valid lemma degraded by memory pressure.** The Lean shim
  reclassifies a `notFound` whose diagnostics carry a resource marker (deep recursion, interrupted, out of memory) to
  `timeout` / `BudgetExceeded`, and the worker child taints a non-positive verdict (never `Accepted`) to `BudgetExceeded`
  when its post-job RSS is at or above an internal, default-off ceiling. A clean name-absent query on a healthy worker
  still returns `NotFound`.
- **`proof_state.locals` now render pretty and notation-aware**, through the same delaborator as `goals_before` /
  `goals_after`, instead of raw `Expr.toString` (`_uniq.NNNN`, ~6 KB/decl — the proximate RSS-spike trigger). A new
  additive `locals_raw: bool` field on the `ProofStateInDeclaration` selector (`#[serde(default)]`, default `false`)
  restores the raw form on request. The verify path and the try-proof-step goals path, which read only `goals_after`,
  now render no locals at all.

No new verification-status variant or protocol version bump: `BudgetExceeded` is reused for the degraded case and
`locals_raw` is backward compatible via `#[serde(default)]`. The internal env var `LEAN_RS_VERIFY_RSS_TAINT_KIB`
(plumbed from the pool's per-worker RSS ceiling) gates the child-side taint and defaults to off. See
[`docs/architecture/09-info-tree-projection.md`](docs/architecture/09-info-tree-projection.md) for the degraded-verdict
semantics and locals rendering modes, and
[`docs/architecture/06-panic-containment.md`](docs/architecture/06-panic-containment.md) for the supervisor
verdict-on-abort contract.

## [0.1.19] - 2026-05-31

### Bounded module-query cost on an incomplete import closure

A module query (`verify_declaration`, `proof_state`, `find_references`, batched selectors) against a file whose import
closure is incomplete (`MissingImports`) no longer elaborates the body. With the imports absent the environment lacks
the file's symbols, so a full elaboration only produced unknown-symbol errors whose projection the caller already
discards as a `needs_build` / `files_skipped` degrade. The Lean shim now short-circuits to an empty projection with
`elaboration_micros = 0`, bounding each per-file query in a project-scope scan to header parsing instead of a full
failing elaboration. No protocol or API change; the `MissingImports` outcome and `missing` list are unchanged. The
remaining cross-query cost on a large scan — cold header re-import after an RSS-driven worker cycle — is documented in
[`docs/architecture/09-info-tree-projection.md`](docs/architecture/09-info-tree-projection.md) with its mitigations
(`limit`, or build the project first).

### Worker robustness: read-only resolution queries inside the panic boundary

A field re-evaluation recorded a one-off child abort (`Lean.MetavarContext.getDecl … unknown metavariable` → `SIGABRT`)
during a genuine-ambiguity `verify_declaration`. A faithful deterministic reproduction on the same toolchain (two `open`
namespaces exporting the same short name, a bare reference forcing Lean's ambiguous-overload error, the offending message
rendered) does not abort the child: the verdict resolves with both candidates and the supervisor performs no restart.
The abort was therefore incidental metavar churn, not a defect of the resolution path. A regression test pins the
no-restart invariant, and [`docs/architecture/06-panic-containment.md`](docs/architecture/06-panic-containment.md)
records why a read-only query is still inside the process panic boundary (a Lean `panic!` under `LEAN_ABORT_ON_PANIC=1`
is uncatchable across the FFI, and relaxing it would let a defaulted value masquerade as a real verdict).

## [0.1.18] - 2026-05-31

### Honest resolution verdicts on the worker query surface (protocol 7 → 8)

The worker's source-snapshot query path (`verify_declaration`, `proof_state`) now reports *why* a name failed to resolve
as a typed verdict instead of an infrastructure-flavoured string. `LeanWorkerProofStateResult` gains
`Ambiguous { candidates }` and `NeedsBuild { missing }`; `Unavailable { message }` is retained only for genuine
non-resolution failures (no proof position matched the selector, snapshot inconsistency), which are not resolution
verdicts. `LeanWorkerDeclarationVerificationStatus` gains `NeedsBuild` (its outcome is the existing
`MissingImports { missing }`, which already names the absent modules — no new field). These replace the previous
behaviour where an unresolved name in an incomplete environment surfaced as `Unavailable { "declaration name is
ambiguous in the module" }` or as `Ambiguous` with no candidates. The verdict is classified once in the Lean shim at the
source-resolution boundary and mapped onto each result, so the rule the consumer applies is purely structural:
`≥ 2 candidates ⇒ ambiguous`, else `needs_build`. Env-based resolution (`inspect_declaration`, `find_references`, search)
is unique by construction and keeps no ambiguity variant.

### Structural candidates for genuine ambiguity

`LeanWorkerDeclarationVerificationFacts` gains `candidates: Vec<LeanWorkerDeclarationTargetInfo>` (`#[serde(default)]`),
populated only when the status is `Ambiguous` (`target` stays `None` then). A candidate carries `namespace_name` as the
disambiguator; no `module` field is added because source-snapshot candidates have no loaded module (it would be `None`
everywhere). Behind this, the Lean shim deduplicates resolution candidates by declaration name before the
arity decision, so a once-defined declaration in a `module` / `@[expose] public section` file resolves cleanly instead
of spuriously reporting `Ambiguous` against identical copies of itself.

### Real axiom collection with a typed availability flag

`verify_declaration` with `report_axioms: true` now runs an actual `Lean.collectAxioms` walk over the resolved
declaration in the elaborated environment (heartbeat- and byte-bounded), replacing the previous textual heuristic that
returned `["sorryAx"]` iff the source mentioned `sorry`/`admit` and otherwise `[]`. `LeanWorkerDeclarationVerificationFacts`
gains `axioms_available: bool` (`#[serde(default)]`): `true` with `axioms: []` means the walk ran and found no
nontrivial axioms; `false` means the walk could not run (target unresolved, budget exhausted). This defines the
misleading empty list — which conflated "checked, none" with "never checked" — out of existence.

### Pretty, notation-aware inspection statements

`LeanWorkerDeclarationInspectionFields` gains `rendering: LeanWorkerRendering` (default `Pretty`), and
`LeanWorkerDeclarationInspection` reports the path actually used via `statement_rendering: Option<LeanWorkerRendering>`.
Under `Pretty` the statement is rendered with `Lean.PrettyPrinter.ppExpr` (notation on, `pp.universes false`) instead of
the raw fully-elaborated `Expr.toString`; on exception or heartbeat exhaustion it falls back to the raw form and reports
`Raw`. The rendering knob is confined to inspection — it is not added to the shared `LeanWorkerRenderedInfo`, which would
over-expose every consumer (goals, type-at) for a knob only inspection uses.

## [0.1.17] - 2026-05-30

### Version-range `cfg` flags for per-toolchain gating

`lean-rs-sys`'s build script now emits a monotone lower-bound `cfg` family alongside the existing exact-version
`lean_v_<token>` flag: `lean_at_least_<major>_<minor>` is set for every supported-window minor at or below the resolved
toolchain (release candidates count as their target minor, so `4.31.0-rc1` ⇒ `lean_at_least_4_31`). Downstream code can
gate a symbol introduced in a given release with `#[cfg(lean_at_least_4_31)]` and the older path with
`#[cfg(not(lean_at_least_4_31))]`, rather than OR-ing exact-version tokens. The script also emits `rustc-check-cfg` for
every version token and minor boundary the window spans, so gating on a non-active version stays lint-clean under
`-D warnings`; only boundaries within the window `[floor ..= head]` are registered.

### Re-export the supported-window query API from `lean-toolchain`

`lean-toolchain` now re-exports `SUPPORTED_TOOLCHAINS`, `SupportedToolchain`, `supported_for`, and `supported_by_digest`
from `lean-rs-sys`, alongside the existing `LEAN_VERSION` / `LEAN_HEADER_DIGEST` / `required_symbols()` surface. This
lets a consumer answer "is this pin inside the supported window?" and "does this `lean.h` digest match a known
toolchain?" through the non-FFI facade, without a direct `lean-rs-sys` dependency (which carries link directives).

### Guarded the bundled interop shim copy against drift

`lean-rs-host`'s bundled `lean-rs-interop-shims` is now a verbatim copy of the canonical package under
`crates/lean-rs/shims/lean-rs-interop-shims`. The host copy had silently drifted — it was missing
`LeanRsInterop.Worker.Stream` (the worker row-streaming helpers), which had been added to the canonical copy alone. The
host copy is meant to be the full generic package, not a host-trimmed subset (it already carried `Callback.String`,
which the host policy shims also do not import), so the missing module was negligent drift, not design. `Worker.Stream`
is added to the host copy, and a new `interop_shims_parity` test asserts the two copies are byte-identical so they
cannot diverge again. The duplication itself is structural — a published crate's `Cargo.toml` `include` cannot reach
into another crate's directory, so each crate vendors its own self-contained copy.

### Removed the obsolete `lake/` shim package mirror

Deleted `lake/lean-rs-host-shims` and `lake/lean-rs-interop-shims`. These were the standalone Lake packages from the
original hybrid layout (v0.1.0), back when downstream consumers `require`d the host shims over git. Once the shim
packages were bundled into the `lean-rs` and `lean-rs-host` crates — shipped via each crate's `Cargo.toml` `include` and
built on demand by the Rust loader — the `lake/` copies became an unreferenced mirror that had drifted stale (they never
received the string-callback C path). Nothing in the build, tests, fixtures, or current docs referenced them. The
authoritative copies remain under `crates/lean-rs/shims/lean-rs-interop-shims`,
`crates/lean-rs-host/shims/lean-rs-interop-shims`, and `crates/lean-rs-host/shims/lean-rs-host-shims`.

### Lean 4.31.0-rc1 added to the supported window

`leanprover/lean4:v4.31.0-rc1` is now a supported and CI-tested toolchain (new `lean.h` digest; layout and
`REQUIRED_SYMBOLS` probes both clean, no missing symbols), and is promoted to the CI/release/sanitizer head. The
supported window is now 4.26.0 through 4.31.0-rc1.

Adopting rc1 also fixed a latent bug in the host "attempt proof" shim, surfaced by Lean 4.31's stricter tactic-block
indentation enforcement. When a candidate tactic was spliced into an existing proof, the bare `by` keyword atom could be
selected as the insertion position and the candidate indented from an unrelated line, emitting it at column 0 — accepted
leniently by Lean ≤ 4.30 but rejected as a parse error by Lean ≥ 4.31. The shim now models a selected proof position as
a type that, by construction, is a real tactic at a positive source column, and derives the candidate's indentation from
that tactic's own column, so insertion aligns with a sibling tactic the parser already accepted. This makes the host's
bounded proof-attempt path behave identically across the supported window.

### Structured worker declaration search

`lean-rs-worker-protocol` replaces the 0.1.16 declaration-search request/row shape with a structured bounded search
surface: name contains/suffix matching, kind filters, required-constant filters, conclusion-head filters, namespace or
module scope biasing, deterministic scores/ranks, compact declaration flags, and fanout/timing facts. Search remains
metadata-only from the caller's perspective and does not render declaration type text; use the existing single-name
declaration-type query for explicit bounded rendering.

## [0.1.16] - 2026-05-27

### Checked export signatures and host boundary tightening

Breaking pre-1.0 change: direct Lean export lookup is now split into a safe checked capability path and an explicit
unsafe unchecked module path. `LeanCapability::exported` validates the requested Rust ABI shape against trusted manifest
signature metadata before dispatch, while arbitrary raw symbol lookup moves to `unsafe LeanModule::exported_unchecked`.
The low-level function-address constructors and raw library symbol resolvers are no longer public API.

`CargoLeanCapability` manifests now use schema version 2 and can record export signatures. The workspace also exposes
the public signature-description types (`LeanExportSignature`, `LeanExportAbiRepr`, ownership/result-convention
metadata, and related values) so downstream build scripts and capability loaders can describe the checked boundary
without duplicating Lean ABI knowledge.

Worker capability dispatch now uses those checked signatures for the closed operation shapes shared by parent, child,
and harness. `lean-rs-worker-parent` re-exports the newer module-query, declaration-search, proof-state, cache, and
budget result types, and the parent/protocol crates now forbid unsafe code.

### Worker module snapshots and declaration lookup

Worker module processing gains a module snapshot cache and bounded declaration lookup surfaces for repeated query
workloads. The new query/result types expose cache status, timings, declaration summaries, target-info results, and
output-budget controls while preserving bounded responses for large modules.

### Lean object decoding and progress callbacks

`lean-rs` adds safe Lean object view helpers for scalar/sum/constructor decoding and a dedicated `LeanProgressCallback`
wrapper for progress callback ABI plumbing. Host-side decoders were migrated onto these safe views, and `lean-rs-host`
no longer contains unsafe code.

## [0.1.15] - 2026-05-26

### Bounded module-query projections

Breaking pre-1.0 change: worker module-processing projections are now bounded by query shape. Diagnostics, cursor type
lookup, cursor goal lookup, and name-reference lookup no longer serialize whole-file raw expression/type strings. Large
module-syntax files that previously killed the worker with `worker protocol frame too large` now return bounded
structured results or explicit truncation.

## [0.1.14]—2026-05-26

### Module-system headers in info-tree processing

`process_module_with_info_tree` now handles Lean 4's module-system headers: the `module` keyword, `public import`,
ordinary private-scope imports, and `import all`. Files using `import all` now resolve the named module instead of
surfacing `unknown module prefix 'all'`, and `userImports` / `missingImports` report the bare module names just as the
legacy header path does.

## [0.1.13]—2026-05-26

### Lake-manifest transitive search paths in shims-only sessions

`lean-rs-host` session imports now add `.olean` search paths for packages listed in the project's `lake-manifest.json`.
Shims-only sessions opened with `LeanHost::load_shims_only()` can import modules from mathlib, batteries, aesop, and
other transitive Lake dependencies without requiring a user `:shared` dylib.

## [0.1.12]—2026-05-26

### Shims-only host sessions

`lean-rs-host` now exposes `LeanHost::load_shims_only()`, a public bootstrap path for hosts that only need the bundled
Meta, elaboration, kernel, declaration, source-range, and info-tree services. It loads the bundled interop and
`LeanRsHostShims` dylibs, resolves the existing session symbols from the shim library, and deliberately skips opening
the user's `:shared` dylib. Sessions still import modules from the project's `.olean` search path; ad-hoc
`LeanSession::call_capability_unchecked` calls return `lean_rs.unsupported` because no user library is attached.

### Worker boundary: shims-only host handle

`lean-rs-worker-parent` now has `LeanWorkerHostHandleBuilder::shims_only(...)` and `LeanWorkerHostHandle` for worker
sessions backed only by bundled host shims. This keeps the existing `LeanWorkerCapabilityBuilder` contract strict for
user `@[export]` dylibs while giving downstream tools a path that does not run `lake build <lib>:shared` before opening
a session. The worker protocol adds `HostSessionMode::{Capability, ShimsOnly}` so the child can route shims-only opens
to `LeanHost::load_shims_only()`.

## [0.1.11]—2026-05-25

### `lean-toolchain`: worker bootstrap accepts `lakefile.toml`

`build_lake_target_with_runner` and its companion helpers (`target_declared_in_lakefile`, `package_name_from_lakefile`)
hardcoded `lakefile.lean`, so any TOML-only Lake project failed worker bootstrap with
`could not read .../lakefile.lean (No such file or directory)`. The bootstrap path now picks the existing lakefile up
front and dispatches declaration and package-name checks on the file extension, so both `lakefile.lean` and
`lakefile.toml` projects bootstrap. TOML parsing consolidates onto the `toml` crate via a new
`lean_toolchain::lakefile_toml` module; the hand-rolled state-machine TOML parser in `modules.rs` is gone.

### `lean-rs-worker-parent`: `Display` now surfaces child stderr on fatal exits

`impl Display for LeanWorkerError` previously rendered `ChildExited` / `ChildPanicOrAbort` as the exit status alone
(`"worker exited fatally with exit status: 1"`). The captured child stderr on `LeanWorkerExit.diagnostics`—populated by
`wait_with_stderr` for exactly this purpose since 0.1.10—was unreachable through the `Display` surface, forcing every
downstream that logs `tracing::error!("{err}")` or stores `err.to_string()` to pattern-match the variants and read the
field by hand.

The `Display` rendering now appends the trimmed stderr tail when non-empty:

```text
worker exited fatally with exit status: 1: could not dlopen X.dylib: image not found
```

Capped at 4 KiB so a runaway Lake / `elan` trace cannot blow up the formatted string; the full text is still available
on `LeanWorkerExit.diagnostics`. The cut respects UTF-8 char boundaries and never lands inside an unterminated ANSI CSI
escape (Lake colourises its trace output). When `diagnostics` is empty, the format collapses back to the original terse
string.

No public-API change: the new helpers are private and the `Display` trait signature is unchanged.

## [0.1.10]—2026-05-25

Re-tagging of 0.1.9. The tag-push release workflow failed in the public-API diff step because the CI's
`cargo-public-api` upgraded from v0.51.0 (which includes parameter names in function signatures) to v0.52.0 (which omits
them); locally regenerated baselines were on v0.51.0 and drifted against the CI run. No crates were published. This
release regenerates every baseline with v0.52.0 and bumps the patch per `docs/release.md` step 7. Functionality
identical to 0.1.9.

## [0.1.9]—2026-05-25

### Worker boundary: configurable per-capability frame cap

`MAX_FRAME_BYTES` is no longer a hard-coded codec ceiling. The parent negotiates a per-connection cap with the child at
handshake time, immediately after the existing `Handshake` frame:

- `Message::ConfigureFrameLimit { max_frame_bytes: u32 }`—new wire variant the parent sends after reading the child's
  `Handshake`; the child installs it on its codec state for the lifetime of the connection.
- `LeanWorkerConfig::max_frame_bytes(n: u32)` and the parallel `LeanWorkerCapabilityBuilder::max_frame_bytes(n: u32)`—
  the supervisor and capability-builder setters. Values are clamped at the public boundary into
  `[MIN_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP]` (64 KiB / 256 MiB), so the child only ever sees a sanitised value.
- `MIN_FRAME_BYTES` and `MAX_FRAME_BYTES_HARD_CAP`—new public constants describing the clamp bounds. `MAX_FRAME_BYTES`
  (1 MiB) keeps its name and value, role changes from "hard limit baked into the codec" to "default cap the supervisor
  applies when no caller overrides".
- `protocol::write_frame` / `protocol::read_frame` gain a `max_frame_bytes: u32` parameter; the codec trusts whatever
  cap the caller (supervisor or child) passes. There is no caller-facing change for callers who do not override the cap.
- `PROTOCOL_VERSION` bumps from 3 to 4—the existing handshake mismatch check is the structural guard against an old↔new
  pairing slipping into the `ConfigureFrameLimit` step.

This makes tools whose single logical result is a frame—outlines of large modules, full-file diagnostics, future "render
the whole info tree" cap shapes—opt into a larger envelope without forking the protocol crate. Existing tools see the
same 1 MiB default.

### `lean-toolchain`: delete `lake_target_declared`, expose `declared_lean_libs`

`lake_target_declared` did a substring scan that only recognised `lakefile.lean` syntax and returned `Ok(false)` for
every `lakefile.toml` project. Its single caller in `lean-rs-worker-parent` already had the answer because
`discover_lake_modules` parses both lakefile formats. The format-aware helper was a parallel implementation of a check
the surrounding code could satisfy from existing state, so it is removed and the planner inlines the lookup against a
new `LeanLakeProjectModules::declared_lean_libs: Vec<String>` field. The field preserves the "explicitly declared by the
lakefile" semantics that motivated the old helper—top-level-fallback projects produce an empty `declared_lean_libs`, so
a loose `Demo.lean` at project root without a matching `lean_lib` is still rejected by the worker bootstrap.

- Added: `LeanLakeProjectModules::declared_lean_libs: Vec<String>`.
- Removed: `lean_toolchain::lake_target_declared` and its unit test.

### `lean-rs-worker-parent`: bind toolchain identity to `LeanWorkerChild`

A worker child binary is built against one Lean toolchain—its rpath points at one `libleanshared`, and `LEAN_SYSROOT` at
spawn time must point at the matching stdlib oleans. The locator now carries both:

- `LeanWorkerChild::for_toolchain(path, sysroot)`—name-and-toolchain constructor.
- `LeanWorkerChild::lean_sysroot(sysroot)`—explicit sysroot setter on an existing locator.

The supervisor sets `LEAN_SYSROOT` from the locator (falling back to `lean_toolchain::discover_toolchain` when no
explicit sysroot is bound) before `Command::spawn`. A single parent process can now host workers for multiple toolchains
by giving each `LeanWorkerCapability` its own `LeanWorkerChild::for_toolchain` locator; downstream consumers
(`lean-host-mcp`) drop their "one server per toolchain" workaround.

`LeanWorkerCapabilityBuilder` deliberately does **not** grow a general `env(key, value)` passthrough—each env var the
worker child needs gets a typed builder method whose name describes the invariant it enforces. The rustdoc on
`LeanWorkerChild` documents this discipline as a load-bearing API contract.

### `lean-rs-worker-parent`: route handshake-error path through `wait_with_stderr`

`LeanWorkerError::Handshake { message }` used to drop the worker child's stderr when the child died mid-bootstrap (e.g.
a bad `LEAN_SYSROOT` aborted the loader before the handshake frame landed). The supervisor's existing `wait_with_stderr`
helper already populates `LeanWorkerExit.diagnostics` with the captured stderr for any post-handshake child crash; the
handshake error path now goes through the same helper. Bootstrap failures surface as `ChildPanicOrAbort { exit }` whose
`exit.diagnostics` carries the underlying loader message, in the same shape as runtime crashes. `Handshake { message }`
survives for the legitimate case where the child completes the handshake but sends a malformed or wrong-version frame.

## [0.1.8]—2026-05-25

### Worker boundary: split into three sibling crates

`lean-rs-worker` is replaced by three crates that separate concerns at the link-graph boundary:

- **`lean-rs-worker-protocol`**—wire types and frame codec. Depends only on `serde` / `serde_json`. Does not link
  `libleanshared`. The `harness` Cargo feature exposes the in-memory frame exerciser and fake-worker test affordances.
  Every public `enum` and field-bearing `struct` is `#[non_exhaustive]` so additive variants do not require a major
  bump.
- **`lean-rs-worker-parent`**—parent-side supervisor, pool, planning, capability, and session. Depends on
  `lean-rs-worker-protocol` and `lean-toolchain`. **Does not link `libleanshared`.** A parent binary that only depends
  on `lean-rs-worker-parent` is free to dispatch to per-toolchain worker children at runtime without being rpath-pinned
  at link time. The crate re-exports the wire types that appear in its public signatures so the common path is a single
  dependency.
- **`lean-rs-worker-child`**—child runtime and the `lean-rs-worker-child` binary. Depends on `lean-rs-worker-protocol`,
  `lean-rs`, `lean-rs-host`, and `lean-toolchain`. The only worker crate that links `libleanshared`. The integration
  tests, examples, and benchmarks that drive a real Lean runtime live here.

The old `lean-rs-worker` crate is removed. There is no shim and no compile_error stub; consumers swap
`lean-rs-worker = "0.1.7"` for `lean-rs-worker-parent = "0.1.8"` (parent process) and `lean-rs-worker-child = "0.1.8"`
(worker binary). Import paths shift from `lean_rs_worker::` to either `lean_rs_worker_parent::` or
`lean_rs_worker_protocol::` depending on whether the type is part of the parent surface or the wire surface. The wire
format and protocol version are unchanged; a 0.1.7 child speaks the same frames as a 0.1.8 parent.

The `lean-rs-worker-child` binary name and on-disk layout are unchanged, so existing
`LeanWorkerChild::sibling("lean-rs-worker-child")` lookups keep working.

### Type promotion to `lean-toolchain`

`LeanBuiltCapability`, `LeanLibraryDependency`, `LeanLoaderDiagnosticCode`, the `LeanCapabilityPreflightReport` /
`LeanCapabilityPreflightCheck` data shapes, and the `LEAN_HEARTBEAT_LIMIT_DEFAULT` /
`LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT` constants now live in `lean-toolchain`. `lean-rs` re-exports them at their existing
paths for workspace source compatibility. This removes the worker boundary's need to mirror host types and the parity
test that gated drift.

### Preflight split

`LeanCapabilityPreflight::check()` is split into two layers. Static manifest validation (file exists, JSON parses,
schema and fingerprint checks) now lives in `lean_toolchain::static_manifest_validation` and runs client-side from
`lean-rs-worker-parent` before any child fork. The heavyweight runtime symbol-table inspection (Mach-O / ELF allowlist
check) stays in `lean-rs` and runs from the child on first command. The wire-visible `LeanCapabilityPreflightReport`
shape is unchanged.

### Workspace

- All published crates bump to `0.1.8`.
- Workspace `Cargo.toml` registers `lean-rs-worker-protocol`, `lean-rs-worker-parent`, and `lean-rs-worker-child`.
- `docs/api-review/` baselines are added for the three new crates; the old `lean-rs-worker-public.txt` is removed.
- The release workflow publishes `lean-rs-worker-protocol` → `lean-rs-worker-parent` → `lean-rs-worker-child` in
  dependency order in place of the old single `lean-rs-worker` publish.

### Migration

```toml
# Cargo.toml—parent process (the common case)
- lean-rs-worker = "0.1.7"
+ lean-rs-worker-parent = "0.1.8"

# Cargo.toml—separate worker binary
+ lean-rs-worker-child = "0.1.8"
```

```rust
// imports—parent surface
- use lean_rs_worker::{LeanWorkerCapability, LeanWorkerCapabilityBuilder, LeanWorkerChild, LeanWorkerPool};
+ use lean_rs_worker_parent::{LeanWorkerCapability, LeanWorkerCapabilityBuilder, LeanWorkerChild, LeanWorkerPool};

// imports—wire types (only when working with the protocol directly)
- use lean_rs_worker::{Request, Response, LeanWorkerElabOptions};
+ use lean_rs_worker_protocol::{Request, Response, LeanWorkerElabOptions};
```

Test-only helpers move from `lean_rs_worker::__test_support` to `lean_rs_worker_protocol::harness` and require the
`harness` feature on `lean-rs-worker-protocol`.

## [0.1.7]—2026-05-24

### `lean-rs-worker` 0.1.7

Two information-loss regressions reported by the first downstream consumer migrating from in-process `LeanSession` to
the worker IPC boundary. Both restore behaviour that the host already implements; the worker now transports it.

- `LeanWorkerKernelResult` now carries `summary: Option<LeanWorkerKernelSummary>`, populated from
  `LeanSession::summarize_evidence` on the `Checked` arm. The `Some iff Checked` invariant is part of the documented
  contract. Restores the proof-summary surface in-process callers relied on before the worker boundary.
- `LeanWorkerSession::infer_type` and `whnf` now attempt notation-aware rendering via the optional `meta_pp_expr` shim
  (`Lean.PrettyPrinter.ppExpr`) and fall back to `Expr.toString` when the shim is absent or reports `Unsupported`. Both
  return `LeanWorkerMetaResult<LeanWorkerRendered>` so callers can see which path was taken via the `rendering`
  (`LeanWorkerRendering::Pretty | Raw`) field. Heartbeat budget is shared with the primary meta call—a slow pretty-print
  on a deeply nested term can in principle starve the meta op (acceptable in practice; `pp_expr` is cheap relative to
  inference).
- A failed `pp_expr` pass (`Failed` / `TimeoutOrHeartbeat`) propagates as the meta call's failure rather than falling
  back to raw. Matches the in-process behaviour the downstream MCP tool already implements.
- Bumped the worker IPC `PROTOCOL_VERSION` from `2` to `3`. The added `summary` field on `LeanWorkerKernelResult` and
  the changed `MetaExpr` payload (`LeanWorkerMetaResult<LeanWorkerRendered>` instead of `<String>`) are
  wire-incompatible with 0.1.6—a mismatched parent/child pair now fails the handshake with a clear
  `LeanWorkerError::Handshake` rather than a cryptic deserialize error on the first request.

Pre-1.0; the changed return types on `infer_type` / `whnf` and the added field on `LeanWorkerKernelResult` break 0.1.6
callers at the call-site. No other crate in the workspace changes.

### Docs

- Removed the stale post-publish step in `docs/release.md` referencing nonexistent `lean-rs-downstream` and
  `lean-rs-host-downstream` proof repos. No such repositories exist under the project; the line was aspirational.

## [0.1.6]—2026-05-24

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
  `process_module`. Each follows the established `elaborate` / `kernel_check` template—`ensure_open` → typed `Request` →
  typed `Response` projection—and invalidates the session on `Cancelled` / `Timeout`.
- The methods compose existing `LeanSession::*` primitives in the child: `infer_type` / `whnf` run a bounded `MetaM`
  call and render the result `LeanExpr` through `LeanSession::expr_to_string_raw` (deterministic, no second MetaM
  round-trip, same shape `ProcessedFile.TermInfo.expr_str` already uses). `describe` composes three primitives—
  `declaration_kind`, `declaration_type`, `declaration_source_range`—into one IPC round-trip.
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
`lean-rs-host`, `lean-rs-worker`, and `lean-toolchain`—17 attributes in total (`LeanError`, `HostStage`,
`LeanDiagnosticCode`, `LeanExceptionKind`, `LeanLoaderDiagnosticCode`, `LeanLoaderSeverity`, `EvidenceStatus`,
`LeanKernelOutcome`, `MetaCallStatus`, `LeanMetaResponse`, `LeanMetaTransparency`, `LeanSeverity`, `ProcessFileOutcome`,
`ProcessModuleOutcome`, `LinkDiagnostics`, `LeanModuleDiscoveryDiagnostic`, `ToolchainInfo`, plus the worker's
pre-existing six). Pre-1.0, no external consumers, and host versions are pinned to the workspace, so the forward-compat
insurance these annotations bought wasn't doing meaningful work. Callers that exhaustively matched these enums can now
rely on the variant set being closed; the worker's `child.rs` defensive wildcard arms over host enums
(`LeanMetaResponse`, `LeanKernelOutcome`, `ProcessFileOutcome`, `ProcessModuleOutcome`, `LeanSeverity`) are gone for the
same reason.

## [0.1.5]—2026-05-23

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

## [0.1.4]—2026-05-23

### `lean-rs-host` 0.1.4

#### Header-aware info-tree projection

- Added `LeanSession::process_module_with_info_tree` and the optional shim `lean_rs_host_process_module_with_info_tree`.
  The new entry point parses a file's header with `Lean.Parser.parseHeader` first, then resumes `IO.processCommands`
  from the parser state—so positions in the returned `ProcessedFile` land in the original file's line/column system with
  no Rust-side offset arithmetic. The previous `process_with_info_tree` shim is still the right call for body-only
  snippets and stays untouched.
- Returns a new outcome enum `ProcessModuleOutcome` with four arms—`Ok { file, imports }`,
  `MissingImports { file, imports, missing }`, `HeaderParseFailed { diagnostics }`, and `Unsupported`. Missing imports
  is a soft failure: the body still elaborates against whatever the env carries, and the partial projection is returned
  for downstream consumers to use.
- Capability contract: mandatory count unchanged (28); optional count 6 → 7. Capability dylibs built against the 0.1.3
  shim set continue to load—the new symbol degrades to `ProcessModuleOutcome::Unsupported`.

### `lean-rs` 0.1.4

#### Runtime initialization

- `LeanRuntime::init` now calls `lean_io_mark_end_initialization()` after the core-runtime + built-in bootstrap. Without
  this call, Lean's `IO.initializing` flag stayed `true` for the process lifetime and any Lean API gated on it (most
  notably `Lean.mkEmptyEnvironment`, transitively used by `Lean.Parser.parseHeader`) threw
  `"environment objects cannot be created during initialization"`. The omission was documented as already-fixed in
  `crates/lean-rs/examples/cold_probe.rs:62` but was missing from the runtime init body. No public API change;
  downstream module initializers loaded via `LeanLibrary::initialize_module` continue to run normally because
  Lake-emitted initializers do not check the flag.

## [0.1.3]—2026-05-23

Adds string projections for opaque `LeanName` and `LeanExpr` handles and a new info-tree projection over processed Lean
sources. Concentrated in `lean-rs-host`; the other four crates have only the toolchain-window extension. The supported
Lean window now covers **4.26.0 through 4.29.1 plus the 4.30.0-rc2 release candidate**.

### `lean-rs-host` 0.1.3

#### `LeanName` rendering

- Added `LeanSession::name_to_string`, `name_to_string_bulk`, and `list_declarations_strings` for projecting opaque
  `LeanName` handles into Rust `String`s. Backed by the new mandatory shim `lean_rs_host_name_to_string`. The handle
  stays opaque—no `Display`, `Eq`, or `From<String>`—so the FFI cost is visible at the call site and the diagnostic-only
  semantics are not papered over.

#### `LeanExpr` rendering

- Added two complementary projections so callers pick the cost tier without a flag.
  - `LeanSession::expr_to_string_raw` walks `Expr.toString` through the new mandatory shim
    `lean_rs_host_env_expr_to_string_raw`. No `MetaM`, no notation, ugly but deterministic—suitable for indexing,
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
  be rebuilt** before they will pass 0.1.3 capability preflight—the two new mandatory entries
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

## [0.1.2]—2026-05-21

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

## [0.1.1]—2026-05-20

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

## [0.1.0]—2026-05-18

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
