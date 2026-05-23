# Host Stack Surface (`lean-rs-host`)

The curated public API of the L2 host stack—the published
[`lean-rs-host`](https://docs.rs/lean-rs-host) crate. The classification table below is the
**semver surface**: which items cross the crate root, which stay at module paths, which stay
`pub(crate)`. Refactors that reshape internal modules are free as long as the curated
re-exports stay stable.

See [`05-raw-sys-design.md`](05-raw-sys-design.md) for `lean-rs-sys`'s sibling rationale.

## Layering

`lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`. The L1 FFI primitive `lean-rs`
ships the typed `@[export]`-calling machinery and the structured error boundary; this L2 crate
adds the opinionated capability/session/pool stack, and depends on the 27 + 4
`lean_rs_host_*` `@[export]` Lean shims bundled with `lean-rs-host` and loaded alongside
consumer capability dylibs.

Downstream applications that just need to call a `@[export]` Lean function with typed
arguments—the norm for Rust bindings to GC-hosted languages—depend on `lean-rs` directly
and skip this crate.

Reusable interop machinery belongs below this crate; see
[`08-reusable-interop.md`](08-reusable-interop.md). The `lean_rs_host_*` shim
contract remains the theorem-prover host policy layer, not the generic callback
or build substrate.

## Curated crate-root surface

```rust
// crates/lean-rs-host/src/lib.rs (illustrative)
pub use crate::host::{
    LeanHost, LeanCapabilities, LeanSession, LeanCancellationToken,
    LeanDeclarationFilter, LeanSourceRange,
    SessionPool, PooledSession, SessionStats, PoolStats,
};
pub use crate::host::elaboration::{
    LeanElabOptions, LeanElabFailure, LeanDiagnostic, LeanSeverity, LeanPosition,
    LEAN_HEARTBEAT_LIMIT_DEFAULT, LEAN_HEARTBEAT_LIMIT_MAX,
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX,
};
pub use crate::host::evidence::{
    LeanEvidence, EvidenceStatus, LeanKernelOutcome, ProofSummary,
    LEAN_PROOF_SUMMARY_BYTE_LIMIT,
};
```

The handle types (`LeanName`, `LeanLevel`, `LeanExpr`, `LeanDeclaration`) and the error model
live on `lean_rs::*`; callers needing both surfaces write `use lean_rs::{LeanRuntime, LeanName}`
and `use lean_rs_host::{LeanHost, LeanSession}` side by side. The L1 crate root re-exports
them; no module-path import needed.

## Specialised sub-module surfaces

The crate root names mandatory session capabilities and entry points only. Sub-module paths
host **specialised or optional** capabilities so the layer difference is visible at the import
site (Ousterhout ch. 7—different layer, different abstraction).

- **`lean_rs_host::meta`**—the bounded `MetaM` capability. Four of the thirty `SessionSymbols` (`meta_infer_type`, `meta_whnf`, `meta_heartbeat_burn`, `meta_is_def_eq`) are optional, and `run_meta` is the only call site that touches the meta types. Surfacing `LeanMetaOptions`, `LeanMetaResponse`, `LeanMetaService`, `LeanMetaTransparency`, `MetaCallStatus`, and the four factories (`infer_type`, `whnf`, `heartbeat_burn`, `is_def_eq`) at the crate root would pollute the namespace of every caller for the benefit of the subset that opts into `MetaM`. Meta callers write `use lean_rs_host::meta::{...}`; everyone else is undisturbed.

## Mandatory entry points

| Item | Module path | Notes |
| --- | --- | --- |
| `LeanHost<'lean>` | `lean_rs_host::host::LeanHost` | Entry point: `from_lake_project(runtime, path)`. |
| `LeanCapabilities<'lean, 'h>` | `lean_rs_host::host::LeanCapabilities` | Loaded capability module reference. |
| `LeanSession<'lean, 'c>` | `lean_rs_host::host::LeanSession` | Long-lived imports and queries; owns bulk/pool methods. |
| `LeanCancellationToken` | `lean_rs_host::host::cancellation::LeanCancellationToken` | `Clone + Send + Sync` cooperative cancellation flag checked by session operations. |
| `LeanProgressSink`, `LeanProgressEvent` | `lean_rs_host::host::progress` | Structured in-thread progress callback surface for long-running session operations. |
| `LeanSourceRange` | `lean_rs_host::host::session::LeanSourceRange` | 1-based source range for a declaration, with Lean-recorded file path or module-label fallback. |
| `LeanDeclarationFilter` | `lean_rs_host::host::session::LeanDeclarationFilter` | User-facing declaration-list filter; default keeps private names and drops generated/internal names. |
| `LeanElabOptions` | `lean_rs_host::host::elaboration::LeanElabOptions` | Bounded options bundle for `elaborate` / `kernel_check`. Saturating setters. |
| `LeanElabFailure` | `lean_rs_host::host::elaboration::LeanElabFailure` | Typed diagnostic payload; carries `&[LeanDiagnostic]` and a `truncated()` flag. |
| `LeanDiagnostic` | `lean_rs_host::host::elaboration::LeanDiagnostic` | One Lean-emitted diagnostic: severity, bounded message, optional position, file label. |
| `LeanSeverity` | `lean_rs_host::host::elaboration::LeanSeverity` | `#[non_exhaustive]` enum mirroring `Lean.MessageSeverity`. |
| `LeanPosition` | `lean_rs_host::host::elaboration::LeanPosition` | 1-indexed line/column with optional end. |
| `LeanEvidence<'lean>` | `lean_rs_host::host::evidence::LeanEvidence` | Opaque checked-evidence handle. Construct via `LeanSession::kernel_check`; operate via `check_evidence` / `summarize_evidence`. |
| `EvidenceStatus` | `lean_rs_host::host::evidence::EvidenceStatus` | `#[non_exhaustive]`: `Checked` / `Rejected` / `Unavailable` / `Unsupported`. |
| `LeanKernelOutcome<'lean>` | `lean_rs_host::host::evidence::LeanKernelOutcome` | Sum returned by `kernel_check`; `LeanEvidence` on `Checked`, `LeanElabFailure` otherwise. |
| `ProofSummary` | `lean_rs_host::host::evidence::ProofSummary` | Lean-authored display projection: declared name, kind string, pretty-printed type signature. Owns bounded `String`s. Diagnostic-only—not a proof certificate outside the session that produced it. |

## Pooling and observability

| Item | Module path | Notes |
| --- | --- | --- |
| `SessionPool<'lean>` | `lean_rs_host::host::pool::SessionPool` | Capacity-bounded reuse pool. `with_capacity(runtime, capacity)`; `acquire(caps, imports) -> PooledSession`; `drain()` drops cached environments. `!Send + !Sync`. |
| `PooledSession<'lean, 'p, 'c>` | `lean_rs_host::host::pool::PooledSession` | `Deref`/`DerefMut` to `LeanSession`; `Drop` returns the environment to the pool (or releases at capacity). |
| `SessionStats` | `lean_rs_host::host::session::SessionStats` | Per-session metrics (`ffi_calls`, `batch_items`, `elapsed_ns`). `Copy + Default + PartialEq`; snapshot semantics. |
| `PoolStats` | `lean_rs_host::host::pool::PoolStats` | Per-pool reuse and drain metrics. `Copy + Default + PartialEq`. |

## Limits

| Constant | Module path | Value |
| --- | --- | ---: |
| `LEAN_HEARTBEAT_LIMIT_DEFAULT` | `host::elaboration` | 200,000 (matches Lean's `maxHeartbeats` at 4.29.1) |
| `LEAN_HEARTBEAT_LIMIT_MAX` | `host::elaboration` | 200,000,000 (saturating ceiling) |
| `LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT` | `host::elaboration` | 64 KiB (per-call cumulative budget) |
| `LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX` | `host::elaboration` | 1 MiB (saturating ceiling) |
| `LEAN_PROOF_SUMMARY_BYTE_LIMIT` | `host::evidence` | 4 KiB per `ProofSummary` field (UTF-8 boundary truncation) |

## Internal types

| Item | Module path | Notes |
| --- | --- | --- |
| `LakeProject` | `lean_rs_host::host::lake::LakeProject` | `pub(crate)`: Lake discovery helper used by `LeanHost`. |
| `meta::{LeanMetaOptions, LeanMetaService, LeanMetaResponse, LeanMetaTransparency, MetaCallStatus, infer_type, whnf, heartbeat_burn, is_def_eq}` | `lean_rs_host::meta::*` (re-exports `host::meta::*`) | Opt-in `MetaM` surface—see *Specialised sub-module surfaces*. |

## Methods on the curated types (happy-path shape)

Each requires doc comments and `# Errors` / `# Panics` sections.

- `LeanHost::from_lake_project(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self>`
- `LeanHost::load_capabilities(&self, package: &str, lib_name: &str) -> LeanResult<LeanCapabilities<'lean, '_>>`—two-arg because Lake's compiled dylib basename is `lib{escaped_package}_{lib_name}.{dylib,so}` with `_` → `__` escaping on the package; both pieces are required to resolve the on-disk artifact.
- `LeanCapabilities::session(&self, imports: &[&str], cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<LeanSession<'lean, '_>>`
- `LeanSession::query_declaration(&mut self, name: &str, cancellation: Option<&LeanCancellationToken>) -> LeanResult<LeanDeclaration<'lean>>`
- `LeanSession::elaborate(&mut self, source: &str, expected_type: Option<&LeanExpr<'lean>>, options: &LeanElabOptions, cancellation: Option<&LeanCancellationToken>) -> LeanResult<Result<LeanExpr<'lean>, LeanElabFailure>>`
- `LeanSession::kernel_check(&mut self, source: &str, options: &LeanElabOptions, cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<LeanKernelOutcome<'lean>>`
- `LeanSession::check_evidence(&mut self, handle: &LeanEvidence<'lean>, cancellation: Option<&LeanCancellationToken>) -> LeanResult<EvidenceStatus>`
- `LeanSession::summarize_evidence(&mut self, handle: &LeanEvidence<'lean>, cancellation: Option<&LeanCancellationToken>) -> LeanResult<ProofSummary>`
- `LeanSession::run_meta<Req, Resp>(&mut self, service: &LeanMetaService<Req, Resp>, request: Req, options: &LeanMetaOptions, cancellation: Option<&LeanCancellationToken>) -> LeanResult<LeanMetaResponse<Resp>>` where `Req: LeanAbi<'lean>` and `Resp: TryFromLean<'lean>`.
- `LeanSession::query_declarations_bulk(&mut self, names: &[&str], cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<Vec<LeanDeclaration<'lean>>>`—strict semantics; first missing name errors the batch with `Host(Conversion)` naming it. With no token and no progress, the method keeps the `N + 1` FFI-call shape (one bulk dispatch + N `name_from_string`) vs `2N` for the singular fold. With a token, it loops through the singular path so it can check between names and discard partial output on cancellation. With progress and no token, it uses one Lean-side progress dispatch.
- `LeanSession::declaration_source_range(&mut self, name: &str, cancellation: Option<&LeanCancellationToken>) -> LeanResult<Option<LeanSourceRange>>`—returns Lean's 1-based declaration range when recorded. The shim resolves module source paths from Lake source roots when possible and falls back to Lean's module label when the source file cannot be found.
- `LeanSession::list_declarations_filtered(&mut self, filter: &LeanDeclarationFilter, cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<Vec<LeanName<'lean>>>`—one Lean-side environment fold that excludes private, generated, and internal names according to the filter before Rust allocates returned handles.
- `LeanSession::declaration_type_bulk(&mut self, names: &[&str], cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<Vec<Option<LeanExpr<'lean>>>>`—with no token, one bulk dispatch over `Array String`; with `Some(token)`, loops through `declaration_type` so cancellation can be checked between names. Missing declarations are `None` in place.
- `LeanSession::declaration_kind_bulk(&mut self, names: &[&str], cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<Vec<String>>`—with no token, one bulk dispatch over `Array String`; with `Some(token)`, loops through `declaration_kind`. Missing declarations are `"missing"` in place.
- `LeanSession::declaration_name_bulk(&mut self, names: &[&str], cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<Vec<String>>`—with no token, one bulk dispatch over `Array String`; with `Some(token)`, loops through `declaration_name`. Missing declarations render as their input name.
- `LeanSession::elaborate_bulk(&mut self, sources: &[&str], options: &LeanElabOptions, cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<Vec<Result<LeanExpr<'lean>, LeanElabFailure>>>`—per-source `Result` mirrors `elaborate` exactly. No `expected_type` parameter; a concrete second caller would justify the per-source `&[Option<&LeanExpr>]` surface. With no token, this is one Lean-side bulk dispatch; with `Some(token)`, this loops per source to create cancellation check points.
- `LeanSession::call_capability<Args, R>(&mut self, name: &str, args: Args, cancellation: Option<&LeanCancellationToken>) -> LeanResult<R::Output>` where `Args: LeanArgs<'lean>` and `R: DecodeCallResult<'lean>`. Function-only escape hatch for capability-dylib exports beyond the twenty-seven session-fixed symbols; reuses the same `Args` / `R` bounds as `lean_rs::LeanModule::exported`, including the `LeanIo<T>` IO marker. Adds session-level tracing (`lean_rs.host.session.call_capability` span with `symbol` + `arity` fields) and a `SessionStats` counter bump—those L2 concerns are why it lives here rather than as a pass-through on `LeanModule`. Callers that don't want the instrumentation use `module.exported::<Args, R>(name)?.call(args)` directly on the L1 handle.
- `LeanSession::stats(&self) -> SessionStats`—snapshot of per-session dispatch metrics.
- `SessionPool::with_capacity(runtime: &'lean LeanRuntime, capacity: usize) -> Self`
- `SessionPool::acquire<'p, 'c>(&'p self, caps: &'c LeanCapabilities<'lean, 'c>, imports: &[&str], cancellation: Option<&LeanCancellationToken>, progress: Option<&dyn LeanProgressSink>) -> LeanResult<PooledSession<'lean, 'p, 'c>>`—capability-agnostic storage; entries are bare `Obj<'lean>` environments rewrapped under the supplied capability borrow at acquire time. FIFO on take, LIFO on push.
- `SessionPool::drain(&self) -> usize`—drops every cached free-list environment and returns the number removed. Checked-out `PooledSession`s remain valid and may return to the pool later. This releases Rust-owned environment refs only; it does not reset Lean's process-global import/runtime state.
- `SessionPool::stats(&self) -> PoolStats`, `len()`, `is_empty()`, `capacity()`—observability. `PoolStats` tracks imports, reuse, release-at-capacity drops, explicit drain calls, and entries removed by drains.

None leak raw `lean_*` types, raw refcount obligations, or initializer-symbol order.

## Lifetime cascade

```rust
LeanRuntime                 ::init() -> LeanResult<&'static LeanRuntime>
LeanHost<'lean>             ::from_lake_project(&'lean LeanRuntime, path) -> ...
LeanCapabilities<'lean, 'h> ::load_capabilities(&'h LeanHost<'lean>, package, lib_name)
LeanSession<'lean, 'c>      ::session(&'c LeanCapabilities<'lean, '_>, imports, cancellation, progress)
LeanExpr<'lean>             // (and the other L1 handles)
```

`'lean` is invisible at typical call sites (inferred from the runtime borrow). Compile-time
enforcement: no handle outlives the runtime borrow; no L2 type escapes to another thread (all
types are `!Send + !Sync` by default—see the trybuild assertion at
`crates/lean-rs-host/tests/compile_fail/runtime_is_not_send_or_sync.rs`).

## Error model

`LeanError` lives on `lean-rs` (L1). It is the only public error type that crosses either
crate's boundary. Two variants: `LeanException(LeanException)` for
Lean-thrown `IO` errors that callers may surface to end users, and `Host(HostFailure)` for any
host-stack failure (init, link, load, conversion, contained callback panic, internal
invariant). Payload structs have private fields and `pub(crate)` constructors that run the
bounding helper, so the `LEAN_ERROR_MESSAGE_LIMIT` (4 KiB) cap on `message()` is a structural
invariant—external callers receive `LeanError` values but cannot mint one with an unbounded
message. `lean-rs-host` constructs `Host(...)` variants via the `lean_rs::__host_internals`
seam.

`Except<E, T>` is a **value type**, not an error. When an exported function returns `IO (Except E T)`:

1. Outer `IO` failure → `LeanError::LeanException` (host failure).
2. Inner `Except` decodes via `TryFromLean` into Rust `Result<T, E>` (application semantics).

The caller sees `LeanResult<Result<T, E>>` and decides how to flatten. Rule: runtime / host
failures are `LeanError`; application semantics are values.

## Consumed L1 surface

`lean-rs-host` reaches into `lean_rs::*` as a normal downstream consumer. None of these belong
to this crate's semver surface—they are listed so the L1 → L2 dependency is visible.

- `LeanRuntime`—process-once init; lifetime anchor for every L2 type.
- `LeanThreadGuard`—RAII attach for worker threads not started inside Lean.
- `LeanLibrary`—RAII handle over the capability dylib `LeanCapabilities` opens.
- `LeanModule`—initialized module handle the session reaches typed exports through.
- `LeanExported`—cached typed function handle the session calls via `call_capability`.
- `LeanArgs`, `LeanIo`, `DecodeCallResult`, `LeanAbi`—bounds and markers for typed dispatch generics.
- `LeanName`, `LeanLevel`, `LeanExpr`, `LeanDeclaration`—opaque handles re-used as L2 method return shapes.
- `lean_rs::error::*`—`LeanError`, `LeanResult`, `LeanException`, `LeanCancelled`, `HostFailure`, `HostStage`, `LeanExceptionKind`, `LeanDiagnosticCode`, `LEAN_ERROR_MESSAGE_LIMIT`, `DiagnosticCapture`, `CapturedEvent`, `DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY`. The shared error model.

Two `#[doc(hidden)] pub` seams substitute for Cargo's missing "friend crate" visibility:

- `lean_rs::__host_internals::{host_module_init,host_cancelled}`—the narrow constructor wrappers this crate calls from `host/lake.rs` and `host/cancellation.rs`. They preserve the bounding invariant (external callers receive `LeanError` values but cannot mint one with an unbounded message) without forcing this crate to re-implement error construction. Re-add wrappers the same way (single-call `#[doc(hidden)] pub fn` + re-export) if a future call site needs one.
- `lean_rs::error::bound_message`—UTF-8-boundary truncation helper used at six sites in `host/{elaboration,meta}/options.rs` and `host/elaboration/diagnostic.rs` to bound Lean-authored strings before they flow into `Host(...)` payloads. Lives on the L1 surface (not at `__host_internals`) because it is a string utility, not a constructor wrapper.

The supertrait `lean_rs::abi::traits::sealed::SealedAbi` is similarly `pub` so this crate can
implement `LeanAbi` for its own host-defined types (`LeanEvidence`, etc.); external crates are
blocked by the orphan rule plus the `#[doc(hidden)]` marker on the parent module.

## Rejected approaches

- **Re-export every `pub` item from every internal module at the crate root.** Path-shortening dressed up as curation; gives mandatory and specialised items equal status. Violates Ousterhout ch. 17 (consistency requires dissimilar things to be done differently).
- **One `LeanHost` god-type with every operation as a method.** The canonical "complect" case (Ousterhout ch. 4 + Hickey): runtime, modules, sessions, semantic handles, and error policy braided into one mechanism. Kills the `'lean` cascade—`LeanExpr<'lean>` cannot outlive its session, but a god type would have to be `'static` to host every method—and forces caller code to thread one large `&mut` through every layer.
- **Hide `lean-rs-host`'s internal modules behind a top-level façade.** Over-encapsulates for hypothetical safety wins. Advanced users already have a clean escape hatch via `lean-rs-sys`, but that drops them to raw FFI. Keeping `lean-rs::module` visible at module paths preserves the middle tier: typed handles, no raw `lean_*` symbols.
- **Keep `LeanHost` / `LeanCapabilities` / `LeanSession` in `lean-rs` itself.** Conflated two layers behind one default entry point and made it impossible for an external L1-only consumer to depend on `lean-rs = "0.1"` without satisfying the 27 + 4 `lean_rs_host_*` shim contract.

## Naming convention

- **Crate-root re-exports use the `Lean` prefix.** Disambiguates `use lean_rs_host::*` in mixed-language projects.
- **Module-path types drop the prefix when the module path disambiguates.** `lean_rs_host::host::session::SessionStats`, `lean_rs_host::host::pool::PoolStats`. If a power-user item is later elevated to the crate root, re-export with the `Lean` prefix where that improves disambiguation (e.g., `pub use ... as LeanSessionStats`).
- **Internal `pub(crate)` types use short names.** They never appear in docs.

## Verification

The classification is satisfied when:

1. `rg -n "^pub use" crates/lean-rs-host/src/lib.rs` matches the curated set above.
2. `crates/lean-rs-host/tests/curated_surface.rs` uses only `use lean_rs::{...}` and `use lean_rs_host::{...}` crate-root items (no module-path access).
3. `crates/lean-rs-host/tests/compile_fail/runtime_is_not_send_or_sync.rs` confirms every L2 type is neither `Send` nor `Sync`.
4. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` is clean and every curated item has a doc comment.
5. `docs/api-review/lean-rs-host-public.txt` matches the curated surface 1:1.
