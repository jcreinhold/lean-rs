# Host stack surface (`lean-rs-host`)

This document pins the curated public API of the L2 host stack —
the published [`lean-rs-host`](https://docs.rs/lean-rs-host) crate.
The table is the **semver surface** for that crate: which items cross
the crate-root boundary, which stay at module paths, and which stay
`pub(crate)`. Refactors that reshape internal modules are free as long
as the curated re-exports below stay stable.

The classification was originally set by `RD-2026-05-17-004` (then
expressed against the monolithic `lean-rs`) and re-anchored at the new
crate by `RD-2026-05-18-001` (the L1/L2 split). See
[`05-raw-sys-design.md`](05-raw-sys-design.md) for the sibling rationale
on `lean-rs-sys`.

## Layering recap

`lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`. The L1
FFI primitive `lean-rs` ships the typed `@[export]`-calling machinery
and the structured error boundary; this L2 crate sits one layer up,
adds the opinionated capability/session/pool stack, and depends on the
13 + 3 `lean_rs_host_*` `@[export]` Lean shims that capabilities load
into the dylib it opens.

Downstream applications that just need to call a `@[export]` Lean
function with typed arguments — the (β)-binding norm — depend on
`lean-rs` directly and skip this crate. See
[`../downstream-integration.md`](../downstream-integration.md) for the
landed L1 proof point and the deferred L2 packaging work.

## Consumed L1 surface

`lean-rs-host` reaches into the following `lean_rs::*` items as a
normal downstream consumer. None of these belong to this crate's
semver surface — they are documented here so the L1 → L2 dependency
graph is visible at a glance:

| Item path                        | Used for                                                                       |
| -------------------------------- | ------------------------------------------------------------------------------ |
| `lean_rs::LeanRuntime`           | Process-once Lean init; lifetime anchor for every L2 type.                     |
| `lean_rs::LeanThreadGuard`       | RAII attach for worker threads that did not start inside Lean.                 |
| `lean_rs::LeanLibrary`           | RAII handle over the capability dylib `LeanCapabilities` opens.                |
| `lean_rs::LeanModule`            | Initialized module handle the session reaches typed exports through.           |
| `lean_rs::LeanExported`          | Cached typed function handle the session calls via `call_capability`.          |
| `lean_rs::LeanArgs`, `LeanIo`, `DecodeCallResult`, `LeanAbi` | Bounds + markers for the typed dispatch generics. |
| `lean_rs::{LeanName, LeanLevel, LeanExpr, LeanDeclaration}`  | Opaque handles re-used as L2 method return shapes. |
| `lean_rs::error::*` (`LeanError`, `LeanResult`, `LeanException`, `HostFailure`, `HostStage`, `LeanExceptionKind`, `LeanDiagnosticCode`, `LEAN_ERROR_MESSAGE_LIMIT`, `DiagnosticCapture`, `CapturedEvent`, `DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY`) | The single error model both crates share; new failure variants land here. |
| `lean_rs::__host_internals::*` (`#[doc(hidden)]`) | Sibling-only seam: `bound_message`, `host_linking`, `host_module_init`, `host_module_init_panic`, `host_symbol_lookup`, `host_callback_panic`, `host_internal`, `lean_exception` — the wrappers that let this crate mint `LeanError::Host(...)` values while preserving the `RD-2026-05-17-006` bounding invariant for true external callers. |

The sealed-trait + `__host_internals` seam (per `RD-2026-05-18-001`)
is `#[doc(hidden)] pub` rather than `pub(crate)` because Cargo does
not have a "friend crate" visibility. External consumers that follow
the obvious `#[doc(hidden)]` signal stay out of the seam; the only
in-tree consumer is this crate.

## Curated crate-root surface

```rust
// crates/lean-rs-host/src/lib.rs (illustrative)
pub use crate::host::{
    LeanHost, LeanCapabilities, LeanSession,
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

The handle types (`LeanName`, `LeanLevel`, `LeanExpr`,
`LeanDeclaration`) and the error model live on `lean_rs::*` after the
split; callers that need both surfaces write
`use lean_rs::{LeanRuntime, LeanName}` and
`use lean_rs_host::{LeanHost, LeanSession}` side by side. The L1 crate
root re-exports them; no module-path import is needed.

## Design it twice — rejected approaches

The curated surface above is one of several shapes considered. Each
alternative was rejected for a specific reason rooted in *A Philosophy
of Software Design* (Ousterhout). Recording them here pins the
rationale so later refactors do not silently drift back.

**Rejected: re-export every `pub` item from every internal module at
the crate root.** This is path-shortening dressed up as curation: every
`lean_rs_host::host::meta::*` item would also be `lean_rs_host::*`,
mandatory entry points and specialised sub-capabilities would gain
equal status, and a future capability addition would silently expand
the root. Violates ch 17 — consistency requires *dissimilar things to
be done differently*.

**Rejected: a single `LeanHost` god-type with every operation as a
method.** Conjoins runtime, module, session, semantic handles, and
error policy into one struct. The canonical "complect" case (ch 4 +
Hickey): the runtime lifetime, module loading, session state, and
handle lifetimes braid into one mechanism. It kills the `'lean`
cascade — `LeanExpr<'lean>` cannot outlive its session, but a god type
would have to be `'static` to host every method — and forces caller
code to thread one large `&mut` handle through every layer.

**Rejected: hide `lean-rs-host`'s internal modules behind a top-level
façade.** Forces every caller through the curated surface, including
the minority that legitimately need a custom `LeanExported` shape for
a capability `lean-rs-host` does not yet wrap. Ch 6 *somewhat
general-purpose, not maximal*: the façade would over-encapsulate,
paying complexity for hypothetical safety wins. Per `RD-2026-05-17-005`,
advanced users already have a clean escape hatch — depend on
`lean-rs-sys` directly — but that drops them all the way to raw FFI.
Keeping `lean-rs::module` visible at module paths preserves a middle
tier: typed handles, no raw `lean_*` symbols.

**Rejected: keep `LeanHost` / `LeanCapabilities` / `LeanSession` in
`lean-rs` itself.** This was the pre-`RD-2026-05-18-001` shape. The
diagnosis: it conflated two layers behind the same default entry point
and made it impossible for an external L1-only consumer to depend on
`lean-rs = "0.1"` without also satisfying the 13 + 3 `lean_rs_host_*`
Lean shim contract that ships nowhere outside the test fixture. See
the survey under `RD-2026-05-18-001` in
`prompts/lean-rs/00-current-state.md` for the (β)-binding architectural
norm that drove the split: every mainstream Rust ↔ GC-language binding
(`ocaml-rs`, `ocaml-interop`, `hs-bindgen`, `caml-oxide`) ships zero
target-language helper code, and Lean is no different at L1.

## Specialised sub-module surfaces

The crate root names mandatory session capabilities and entry points
only. Sub-module paths host **specialised or optional** capabilities so
that the layer difference is visible at the import site (ch 7 —
different layer, different abstraction).

- **`lean_rs_host::meta`** — the bounded `MetaM` capability. Three of
    the fourteen `SessionSymbols` (the `meta_infer_type`, `meta_whnf`,
    `meta_heartbeat_burn` addresses in `host/session.rs`) are optional,
    and `run_meta` is the only call site that touches the meta types.
    Surfacing `LeanMetaOptions`, `LeanMetaResponse`, `LeanMetaService`,
    `LeanMetaTransparency`, `MetaCallStatus`, and the three factory
    functions (`infer_type`, `whnf`, `heartbeat_burn`) at the crate
    root would pollute the namespace of every caller for the benefit of
    the subset that opts in to `MetaM`. Callers that need meta write
    `use lean_rs_host::meta::{...}`; everyone else is undisturbed.

## Classification table

L2 surface only — L1 items consumed by this crate are listed in
*Consumed L1 surface* above.

| Item                                              | Module path                                            | Crate-root re-export?               | Visibility | Notes                                                                                                                                                  |
| ------------------------------------------------- | ------------------------------------------------------ | ----------------------------------- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `LeanHost<'lean>`                                 | `lean_rs_host::host::LeanHost`                         | yes (`lean_rs_host::LeanHost`)      | `pub`      | Entry point. `from_lake_project(runtime, path)`.                                                                                                       |
| `LeanCapabilities<'lean, 'h>`                     | `lean_rs_host::host::LeanCapabilities`                 | yes                                 | `pub`      | Loaded capability module reference.                                                                                                                    |
| `LeanSession<'lean, 'c>`                          | `lean_rs_host::host::LeanSession`                      | yes                                 | `pub`      | Long-lived imports + queries; **owns batch/bulk methods**.                                                                                             |
| `SessionStats`                                    | `lean_rs_host::host::session::SessionStats`            | yes                                 | `pub`      | Per-session dispatch metrics (ffi_calls, batch_items, elapsed_ns) returned by `LeanSession::stats()`. `Copy + Default + PartialEq`; snapshot semantics. |
| `SessionPool<'lean>`                              | `lean_rs_host::host::pool::SessionPool`                | yes                                 | `pub`      | Capacity-bounded reuse pool of imported Lean environments. `with_capacity(runtime, capacity)`; `acquire(caps, imports)` returns a `PooledSession`. `!Send + !Sync`. |
| `PooledSession<'lean, 'p, 'c>`                    | `lean_rs_host::host::pool::PooledSession`              | yes                                 | `pub`      | A `LeanSession` borrowed from a [`SessionPool`]; `Deref`/`DerefMut` to `LeanSession`; `Drop` returns the environment to the pool (or releases if at capacity). |
| `PoolStats`                                       | `lean_rs_host::host::pool::PoolStats`                  | yes                                 | `pub`      | Per-pool reuse metrics (imports_performed, reused, acquired, released_to_pool, released_dropped). `Copy + Default + PartialEq`; snapshot semantics. |
| `LeanEvidence<'lean>`                             | `lean_rs_host::host::evidence::LeanEvidence`           | yes                                 | `pub`      | Opaque checked-evidence handle. Construct via [`LeanSession::kernel_check`]; no public inherent methods. Operate via [`LeanSession::check_evidence`] and [`LeanSession::summarize_evidence`]. |
| `EvidenceStatus`                                  | `lean_rs_host::host::evidence::EvidenceStatus`         | yes                                 | `pub`      | Tag enum: `Checked` / `Rejected` / `Unavailable` / `Unsupported`. `#[non_exhaustive]`.                                                                  |
| `LeanKernelOutcome<'lean>`                        | `lean_rs_host::host::evidence::LeanKernelOutcome`      | yes                                 | `pub`      | Sum returned by [`LeanSession::kernel_check`]; carries a [`LeanEvidence`] on `Checked` or a [`LeanElabFailure`] on the other three variants.            |
| `ProofSummary`                                    | `lean_rs_host::host::evidence::ProofSummary`           | yes                                 | `pub`      | Lean-authored display projection of a [`LeanEvidence`]: declared name, kind string, pretty-printed type signature. Owns only bounded `String`s (no `'lean`). Fields are diagnostic-only — not semantic keys and not proof certificates outside the session that produced or validated them. |
| `LEAN_PROOF_SUMMARY_BYTE_LIMIT`                   | `lean_rs_host::host::evidence::LEAN_PROOF_SUMMARY_BYTE_LIMIT` | yes                          | `pub const`| 4 KiB upper bound the Lean-side summariser enforces on each `ProofSummary` field; truncates at a UTF-8 character boundary.                              |
| `LeanElabOptions`                                 | `lean_rs_host::host::elaboration::LeanElabOptions`     | yes                                 | `pub`      | Bounded options bundle for `elaborate` / `kernel_check`: heartbeat limit, diagnostic byte limit, namespace context, file label. Setters saturate.       |
| `LeanElabFailure`                                 | `lean_rs_host::host::elaboration::LeanElabFailure`     | yes                                 | `pub`      | Typed diagnostic payload returned by `elaborate` / non-`Checked` `kernel_check`. Carries an ordered `&[LeanDiagnostic]` and a `truncated()` flag.       |
| `LeanDiagnostic`                                  | `lean_rs_host::host::elaboration::LeanDiagnostic`      | yes                                 | `pub`      | One Lean-emitted diagnostic: severity, bounded message, optional position, file label.                                                                  |
| `LeanSeverity`                                    | `lean_rs_host::host::elaboration::LeanSeverity`        | yes                                 | `pub`      | Tag enum mirroring `Lean.MessageSeverity`: `Info` / `Warning` / `Error`. `#[non_exhaustive]`.                                                            |
| `LeanPosition`                                    | `lean_rs_host::host::elaboration::LeanPosition`        | yes                                 | `pub`      | 1-indexed `line` / `column`, optional end `line` / `column`. Mirrors Lean's `Position` shape.                                                            |
| `LEAN_HEARTBEAT_LIMIT_DEFAULT`                    | `lean_rs_host::host::elaboration::LEAN_HEARTBEAT_LIMIT_DEFAULT` | yes                        | `pub const`| 200_000 — matches Lean's own `maxHeartbeats` default at 4.29.1.                                                                                          |
| `LEAN_HEARTBEAT_LIMIT_MAX`                        | `lean_rs_host::host::elaboration::LEAN_HEARTBEAT_LIMIT_MAX`     | yes                        | `pub const`| 200_000_000 ceiling for the heartbeat setter; saturating.                                                                                                |
| `LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT`              | `lean_rs_host::host::elaboration::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT` | yes                  | `pub const`| 64 KiB — default cumulative diagnostic byte budget per call.                                                                                             |
| `LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`                  | `lean_rs_host::host::elaboration::LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`     | yes                  | `pub const`| 1 MiB ceiling for the diagnostic byte budget setter; saturating.                                                                                         |
| `LakeProject`                                     | `lean_rs_host::host::lake::LakeProject`                | no                                  | `pub(crate)` | Lake discovery helper used by `LeanHost`.                                                                                                              |
| `meta::{LeanMetaOptions, LeanMetaService, LeanMetaResponse, LeanMetaTransparency, MetaCallStatus, infer_type, whnf, heartbeat_burn}` | `lean_rs_host::meta::*` (re-exporting `lean_rs_host::host::meta::*`) | no | `pub` | Opt-in `MetaM` surface (see *Specialised sub-module surfaces*). |

## Naming convention

- **Crate-root re-exports use the `Lean` prefix.** Disambiguates
    `use lean_rs_host::*` in mixed-language projects.
- **Module-path types drop the prefix when the module path disambiguates.**
    `lean_rs_host::host::session::SessionStats`,
    `lean_rs_host::host::pool::PoolStats`. If a power-user item is later
    elevated to the crate root, re-export it with the `Lean` prefix if
    that improves disambiguation (e.g., `pub use ... as LeanSessionStats`).
- **Internal `pub(crate)` types use short names.** They never appear in docs.

## Lifetime cascade

The `'lean` lifetime parameter from `lean-rs` cascades through every
type that holds a Lean object:

```rust
LeanRuntime                 ::init() -> LeanResult<&'static LeanRuntime>
LeanHost<'lean>             ::from_lake_project(&'lean LeanRuntime, path) -> ...
LeanCapabilities<'lean, 'h> ::load_capabilities(&'h LeanHost<'lean>, package, lib_name)
LeanSession<'lean, 'c>      ::session(&'c LeanCapabilities<'lean, '_>, imports)
LeanExpr<'lean>             // (and the other L1 handles)
```

The `'lean` parameter is invisible at a typical call site (inferred
from the runtime borrow). Compile-time enforcement: no handle can
outlive the runtime borrow, no L2 type escapes to another thread (all
types are `!Send + !Sync` by default — see the trybuild assertion at
`crates/lean-rs-host/tests/compile_fail/runtime_is_not_send_or_sync.rs`).

## Methods on the curated types (happy-path shape)

The methods named here exist on the curated types. Doc comments and
`# Errors` / `# Panics` sections are mandatory.

- `LeanHost::from_lake_project(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self>`
- `LeanHost::load_capabilities(&self, package: &str, lib_name: &str) -> LeanResult<LeanCapabilities<'lean, '_>>`
    (two-arg because Lake's compiled dylib basename is
    `lib{escaped_package}_{lib_name}.{dylib,so}` with `_` → `__`
    escaping on the package; both pieces are required to resolve the
    on-disk artifact)
- `LeanCapabilities::session(&self, imports: &[&str]) -> LeanResult<LeanSession<'lean, '_>>`
- `LeanSession::query_declaration(&mut self, name: &str) -> LeanResult<LeanDeclaration<'lean>>`
- `LeanSession::elaborate(&mut self, source: &str, expected_type: Option<&LeanExpr<'lean>>, options: &LeanElabOptions) -> LeanResult<Result<LeanExpr<'lean>, LeanElabFailure>>`
- `LeanSession::kernel_check(&mut self, source: &str, options: &LeanElabOptions) -> LeanResult<LeanKernelOutcome<'lean>>`
- `LeanSession::check_evidence(&mut self, handle: &LeanEvidence<'lean>) -> LeanResult<EvidenceStatus>`
- `LeanSession::summarize_evidence(&mut self, handle: &LeanEvidence<'lean>) -> LeanResult<ProofSummary>`
- `LeanSession::run_meta<Req, Resp>(&mut self, service: &LeanMetaService<Req, Resp>, request: Req, options: &LeanMetaOptions) -> LeanResult<LeanMetaResponse<Resp>>`
    where `Req: LeanAbi<'lean>` and `Resp: TryFromLean<'lean>`.
- `LeanSession::query_declarations_bulk(&mut self, names: &[&str]) -> LeanResult<Vec<LeanDeclaration<'lean>>>`
    — strict semantics; first missing name errors the batch with
    `Host(Conversion)` naming it. Costs `N + 1` FFI calls (one bulk
    dispatch + `N` `name_from_string`) vs. `2 * N` for the singular fold.
- `LeanSession::elaborate_bulk(&mut self, sources: &[&str], options: &LeanElabOptions) -> LeanResult<Vec<Result<LeanExpr<'lean>, LeanElabFailure>>>`
    — per-source `Result` mirrors `LeanSession::elaborate` exactly. No
    `expected_type` parameter — deferred until a real second caller
    earns the per-source `&[Option<&LeanExpr>]` surface.
- `LeanSession::call_capability<Args, R>(&mut self, name: &str, args: Args) -> LeanResult<R::Output>`
    where `Args: LeanArgs<'lean>` and `R: DecodeCallResult<'lean>`.
    Function-only escape hatch for invoking capability-dylib exports
    beyond the thirteen session-fixed symbols; reuses the same
    `Args` / `R` bounds as `lean_rs::LeanModule::exported`, including
    the `LeanIo<T>` IO marker. **Adds session-level tracing
    (`lean_rs.host.session.call_capability` span with `symbol` +
    `arity` fields) and a `SessionStats` counter bump** — those L2
    concerns are why it lives here rather than as a pass-through on
    `LeanModule`. Callers that don't want the instrumentation use
    `module.exported::<Args, R>(name)?.call(args)` directly on the L1
    handle.
- `LeanSession::stats(&self) -> SessionStats` — snapshot of
    per-session dispatch metrics.
- `SessionPool::with_capacity(runtime: &'lean LeanRuntime, capacity: usize) -> Self`
- `SessionPool::acquire<'p, 'c>(&'p self, caps: &'c LeanCapabilities<'lean, 'c>, imports: &[&str]) -> LeanResult<PooledSession<'lean, 'p, 'c>>`
    — capability-agnostic storage; entries are bare `Obj<'lean>`
    environments rewrapped under the supplied capability borrow at
    acquire time. FIFO on take, LIFO on push; matches the `imports`-list
    cache key structurally.
- `SessionPool::stats(&self) -> PoolStats`, `len()`, `is_empty()`,
    `capacity()` — observability.

None leak raw `lean_*` types, raw refcount obligations, or
initializer-symbol order.

## Error model

`LeanError` lives on `lean-rs` (the L1 crate). It is the only public
error type that crosses the boundary on either crate. Per
`RD-2026-05-17-006`, it has two variants:
`LeanException(LeanException)` for Lean-thrown `IO` errors that callers
may surface to end users, and `Host(HostFailure)` for any failure of
the host stack (init, link, load, conversion, contained callback
panic, internal invariant). The payload structs have private fields
and `pub(crate)` constructors that run the bounding helper, so the
`LEAN_ERROR_MESSAGE_LIMIT` (4 KiB) cap on `message()` is a structural
invariant rather than convention — external callers receive `LeanError`
values but cannot mint one with an unbounded message. `lean-rs-host`
constructs `Host(...)` variants via the `lean_rs::__host_internals`
seam, which routes through the same bounding helper.

`Except<E, T>` is a **value type**, not an error. When an exported
function returns `IO (Except E T)`:

1. Outer `IO` failure → `LeanError::LeanException` (host failure).
1. Inner `Except` decodes via `TryFromLean` into Rust
    `Result<T, E>` (application semantics).

The caller sees `LeanResult<Result<T, E>>` and decides how to flatten.
The rule: **runtime / host failures are `LeanError`; application
semantics are values.**

## Verification

The classification table is satisfied when:

1. `rg -n "^pub use" crates/lean-rs-host/src/lib.rs` matches the
    curated set above.
2. The integration test
    `crates/lean-rs-host/tests/curated_surface.rs` uses only
    `use lean_rs::{...}` and `use lean_rs_host::{...}` crate-root
    items (no module-path access).
3. The trybuild test
    `crates/lean-rs-host/tests/compile_fail/runtime_is_not_send_or_sync.rs`
    confirms every L2 type is neither `Send` nor `Sync`.
4. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` is
    clean and every curated item has a doc comment.
5. `docs/api-review/lean-rs-host-public.txt` matches the curated
    surface 1:1.
