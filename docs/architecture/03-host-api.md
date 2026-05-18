# Host API surface

This document pins the curated public API of `lean-rs`: which items cross the crate-root boundary, which stay at module
paths, and which stay `pub(crate)`. The table is the **semver surface** for the published crate. Refactors that reshape
internal modules are free as long as the curated re-exports below stay stable.

The classification is set in advance by `RD-2026-05-17-004` (see also [`05-raw-sys-design.md`](05-raw-sys-design.md) for
the sibling rationale on `lean-rs-sys`) so that prompt 18 has a table to verify against, not to invent. Implementation
prompts 06–17 must land items consistent with this table.

## Layering recap

`lean-rs-sys` → `lean-toolchain` → `lean-rs`. Inside `lean-rs`:

- `error` (pub): `LeanError`, `LeanResult`, sub-error structs.
- `module` (pub): typed exported-function loading.
- `host` (pub): semantic sessions, capabilities, opaque handles, evidence.
- `runtime` (pub(crate)): init, `Obj<'lean>`, thread guards.
- `abi` (pub(crate)): scalar / string / array / option / except conversions.

Batch operations are methods on `LeanSession`; session pooling is a sibling helper at `lean_rs::host::pool::SessionPool`
(re-exported at the crate root) — both shape choices follow `RD-2026-05-17-004`. There is no `lean_rs::batch` module.
The pool's storage state and capacity policy are self-contained enough to earn its own file but not its own module
boundary: it speaks only to `LeanSession`'s `pub(crate) from_environment` / `into_environment` helpers and to the
caller-supplied `LeanCapabilities` at acquire time.

## Curated crate-root surface

```rust
// crates/lean-rs/src/lib.rs (illustrative — landed by prompt 18, extended by prompt 20)
pub use crate::error::{
    LeanError, LeanResult,
    LeanException, HostFailure,
    HostStage, LeanExceptionKind,
    LEAN_ERROR_MESSAGE_LIMIT,
};
pub use crate::host::{
    LeanHost, LeanCapabilities, LeanSession,
    SessionPool, PooledSession, SessionStats, PoolStats,
};
pub use crate::host::handle::{LeanName, LeanLevel, LeanExpr, LeanDeclaration};
pub use crate::host::elaboration::{
    LeanElabOptions, LeanElabFailure, LeanDiagnostic, LeanSeverity, LeanPosition,
    LEAN_HEARTBEAT_LIMIT_DEFAULT, LEAN_HEARTBEAT_LIMIT_MAX,
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX,
};
pub use crate::host::evidence::{LeanEvidence, EvidenceStatus, LeanKernelOutcome, ProofSummary};
pub use crate::runtime::LeanRuntime;
```

`LeanModule` and the `LeanExported{N}` family stay at `lean_rs::module::*` for power users; they are not at the crate
root because the happy path goes through `LeanHost` → `LeanCapabilities` → `LeanSession`.

## Design it twice — rejected approaches

The curated surface above is one of several shapes considered for prompt 18. Each alternative was rejected for a
specific reason rooted in *A Philosophy of Software Design* (Ousterhout). Recording them here pins the rationale so
later refactors do not silently drift back.

**Rejected: re-export every `pub` item from every internal module at the crate root.** This is path-shortening
dressed up as curation: every `lean_rs::module::*` item would also be `lean_rs::*`, every `host::meta::*` item
would gain a second name. It teaches no sequence (the `LeanExported` loader sits adjacent to `LeanRuntime::init`,
which is wrong) and violates ch 17 — consistency requires *dissimilar things to be done differently*, but the
flat re-export gives mandatory entry points and specialized sub-capabilities equal status. A future capability
addition would silently expand the root.

**Rejected: a single `Lean` god-type with every operation as a method.** Conjoins runtime, module, session,
semantic handles, and error policy into one struct. This is the canonical "complect" case (ch 4 + Hickey): two
or more concerns that change independently — runtime lifetime, module loading, session state, handle lifetimes —
braided into one mechanism. It kills the `'lean` cascade (`LeanExpr<'lean>` cannot outlive its session, but a god
type would have to be `'static` to host every method), forces caller code to thread a single mutable handle
through every layer, and turns simple per-handle borrows into one large `&mut Lean` that serialises every call.

**Rejected: hide `lean_rs::runtime`, `::module`, `::host` entirely behind a top-level façade.** Forces every
caller through the curated surface, including the minority that legitimately need raw module loading or a custom
`LeanExported` shape for a capability `lean-rs` does not yet wrap. Ch 6 "somewhat general-purpose, not maximal":
the façade would over-encapsulate, paying complexity for hypothetical safety wins. Per `RD-2026-05-17-005`,
advanced users already have a clean escape hatch — depend on `lean-rs-sys` directly — but that drops them all
the way to raw FFI. Keeping `lean_rs::module` and `lean_rs::host` visible at module paths preserves a middle
tier: typed handles, no raw `lean_*` symbols.

## Specialized sub-module surfaces

The crate root names mandatory session capabilities and entry points only. Sub-module paths host **specialized
or optional** capabilities so that the layer difference is visible at the import site (ch 7 — different layer,
different abstraction).

- **`lean_rs::host::meta`** — the bounded `MetaM` capability. Three of the fourteen `SessionSymbols` (the
    `meta_infer_type`, `meta_whnf`, `meta_heartbeat_burn` addresses in `host/session.rs`) are optional, and
    `run_meta` is the only call site that touches the meta types. Surfacing `LeanMetaOptions`,
    `LeanMetaResponse`, `LeanMetaService`, `LeanMetaTransparency`, `MetaCallStatus`, and the three factory
    functions (`infer_type`, `whnf`, `heartbeat_burn`) at the crate root would pollute the namespace of every
    caller for the benefit of the subset that opts in to `MetaM`. Callers that need meta write
    `use lean_rs::host::meta::{...}`; everyone else is undisturbed.
- **`lean_rs::module`** — the typed exported-function loader (`LeanLibrary`, `LeanModule`, `LeanExported`,
    `LeanIo`, `LeanArgs`, `DecodeCallResult`, `LeanAbi`). The happy path runs through `LeanHost` →
    `LeanCapabilities` → `LeanSession`, which wraps module loading; only embedders calling a not-yet-wrapped
    capability need the typed loader directly.

## Classification table

| Item                                              | Module path                                 | Crate-root re-export?        | Visibility   | Notes                                                                                                                                                  |
| ------------------------------------------------- | ------------------------------------------- | ---------------------------- | ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `LeanRuntime`                                     | `lean_rs::runtime::LeanRuntime`             | yes (`lean_rs::LeanRuntime`) | `pub`        | Process-once init via `OnceLock`; `!Send + !Sync` ZST. `init() -> LeanResult<&'static LeanRuntime>`.                                                   |
| `Obj<'lean>`                                      | `lean_rs::runtime::obj::Obj`                | no                           | `pub(crate)` | Universal currency. `Drop`=`lean_dec`, `Clone`=`lean_inc`.                                                                                             |
| `ObjRef<'lean, 'a>`                               | `lean_rs::runtime::obj::ObjRef`             | no                           | `pub(crate)` | Borrowed view; tied to both runtime and owner lifetimes.                                                                                               |
| `LeanThreadGuard<'lean>`                          | `lean_rs::runtime::thread::LeanThreadGuard` | optional                     | `pub`        | RAII thread attach for worker threads. Re-export TBD by prompt 24.                                                                                     |
| `IntoLean<'lean>` (trait)                         | `lean_rs::abi::IntoLean`                    | no                           | `pub(crate)` | Conversion machinery; never escapes.                                                                                                                   |
| `TryFromLean<'lean>` (trait)                      | `lean_rs::abi::TryFromLean`                 | no                           | `pub(crate)` | Conversion machinery; never escapes.                                                                                                                   |
| `Except<E, T>`                                    | `lean_rs::abi::except::Except`              | no                           | `pub(crate)` | Rust mirror of Lean `Except`; used internally for IO decoding.                                                                                         |
| `LeanModule<'lean, 'lib>`                         | `lean_rs::module::LeanModule`               | no                           | `pub`        | Loaded + initialized Lean module; borrows its `LeanLibrary<'lean>` via `'lib` so the dylib outlives the handle.                                        |
| `LeanLibrary<'lean>`                              | `lean_rs::module::LeanLibrary`              | no                           | `pub`        | RAII handle over a Lake-built `.so`/`.dylib`.                                                                                                          |
| `LeanExported<'lean, 'lib, Args, R>`              | `lean_rs::module::LeanExported`             | no                           | `pub`        | Single generic typed function handle; `.call` impl is macro-stamped per arity 0..=12. `Args` is a tuple of types implementing `LeanAbi`; `R` is bounded by `DecodeCallResult`. Per `RD-2026-05-17-007`.                              |
| `LeanIo<T>`                                       | `lean_rs::module::LeanIo`                   | no                           | `pub`        | Return-type marker for `IO α` exports; writing `exported::<Args, LeanIo<T>>(name)` composes `decode_io` before `T::try_from_lean`. Cannot be constructed by callers (private field).                                                |
| `LeanAbi<'lean>` (trait)                          | `lean_rs::abi::LeanAbi` (re-exported at `lean_rs::module::LeanAbi`) | no | `pub` (sealed) | Per-type C-ABI representation: `type CRepr; fn into_c; fn from_c`. Lake emits unboxed scalars for `Bool`/`UIntN`/`Char`/`Float` and `lean_object*` for everything boxed; `LeanAbi` hides which convention applies. Sealed.            |
| `LeanArgs<'lean>` (trait)                         | `lean_rs::module::LeanArgs`                 | no                           | `pub` (sealed) | Arity marker on argument tuples (`const ARITY: usize`). Sealed.                                                                                                                                                                       |
| `DecodeCallResult<'lean>` (trait)                 | `lean_rs::module::DecodeCallResult`         | no                           | `pub` (sealed) | Return-type decoder; two impls (pure: `T: LeanAbi`, IO: `LeanIo<T>` for `T: TryFromLean`). Sealed.                                                                                                                                    |
| `LeanHost<'lean>`                                 | `lean_rs::host::LeanHost`                   | yes                          | `pub`        | Entry point. `from_lake_project(runtime, path)`.                                                                                                       |
| `LeanCapabilities<'lean, 'h>`                     | `lean_rs::host::LeanCapabilities`           | yes                          | `pub`        | Loaded capability module reference.                                                                                                                    |
| `LeanSession<'lean, 'c>`                          | `lean_rs::host::LeanSession`                | yes                          | `pub`        | Long-lived imports + queries; **owns batch/bulk methods**.                                                                                             |
| `SessionStats`                                    | `lean_rs::host::session::SessionStats`      | yes                          | `pub`        | Per-session dispatch metrics (ffi_calls, batch_items, elapsed_ns) returned by `LeanSession::stats()`. `Copy + Default + PartialEq`; snapshot semantics. |
| `SessionPool<'lean>`                              | `lean_rs::host::pool::SessionPool`          | yes                          | `pub`        | Capacity-bounded reuse pool of imported Lean environments. `with_capacity(runtime, capacity)`; `acquire(caps, imports)` returns a `PooledSession`. `!Send + !Sync`. |
| `PooledSession<'lean, 'p, 'c>`                    | `lean_rs::host::pool::PooledSession`        | yes                          | `pub`        | A `LeanSession` borrowed from a [`SessionPool`]; `Deref`/`DerefMut` to `LeanSession`; `Drop` returns the environment to the pool (or releases if at capacity). |
| `PoolStats`                                       | `lean_rs::host::pool::PoolStats`            | yes                          | `pub`        | Per-pool reuse metrics (imports_performed, reused, acquired, released_to_pool, released_dropped). `Copy + Default + PartialEq`; snapshot semantics. |
| `LeanName<'lean>`                                 | `lean_rs::host::handle::LeanName`           | yes                          | `pub`        | Opaque semantic handle.                                                                                                                                |
| `LeanLevel<'lean>`                                | `lean_rs::host::handle::LeanLevel`          | yes                          | `pub`        | Opaque semantic handle.                                                                                                                                |
| `LeanExpr<'lean>`                                 | `lean_rs::host::handle::LeanExpr`           | yes                          | `pub`        | Opaque semantic handle.                                                                                                                                |
| `LeanDeclaration<'lean>`                          | `lean_rs::host::handle::LeanDeclaration`    | yes                          | `pub`        | Opaque semantic handle.                                                                                                                                |
| `LeanEvidence<'lean>`                             | `lean_rs::host::evidence::LeanEvidence`     | yes                          | `pub`        | Opaque checked-evidence handle. Construct via [`LeanSession::kernel_check`]; no public inherent methods. Operate via [`LeanSession::check_evidence`] and [`LeanSession::summarize_evidence`]. |
| `EvidenceStatus`                                  | `lean_rs::host::evidence::EvidenceStatus`   | yes                          | `pub`        | Tag enum: `Checked` / `Rejected` / `Unavailable` / `Unsupported`. `#[non_exhaustive]`.                                                                  |
| `LeanKernelOutcome<'lean>`                        | `lean_rs::host::evidence::LeanKernelOutcome`| yes                          | `pub`        | Sum returned by [`LeanSession::kernel_check`]; carries a [`LeanEvidence`] on `Checked` or a [`LeanElabFailure`] on the other three variants.            |
| `ProofSummary`                                    | `lean_rs::host::evidence::ProofSummary`     | yes                          | `pub`        | Lean-authored display projection of a [`LeanEvidence`]: declared name, kind string, pretty-printed type signature. Owns only bounded `String`s (no `'lean`). Fields are diagnostic-only — not semantic keys and not proof certificates outside the session that produced or validated them. |
| `LEAN_PROOF_SUMMARY_BYTE_LIMIT`                   | `lean_rs::host::evidence::LEAN_PROOF_SUMMARY_BYTE_LIMIT` | yes             | `pub const`  | 4 KiB upper bound the Lean-side summariser enforces on each `ProofSummary` field; truncates at a UTF-8 character boundary.                              |
| `LeanElabOptions`                                 | `lean_rs::host::elaboration::LeanElabOptions` | yes                        | `pub`        | Bounded options bundle for `elaborate` / `kernel_check`: heartbeat limit, diagnostic byte limit, namespace context, file label. Setters saturate.       |
| `LeanElabFailure`                                 | `lean_rs::host::elaboration::LeanElabFailure` | yes                        | `pub`        | Typed diagnostic payload returned by `elaborate` / non-`Checked` `kernel_check`. Carries an ordered `&[LeanDiagnostic]` and a `truncated()` flag.       |
| `LeanDiagnostic`                                  | `lean_rs::host::elaboration::LeanDiagnostic`  | yes                        | `pub`        | One Lean-emitted diagnostic: severity, bounded message, optional position, file label.                                                                  |
| `LeanSeverity`                                    | `lean_rs::host::elaboration::LeanSeverity`    | yes                        | `pub`        | Tag enum mirroring `Lean.MessageSeverity`: `Info` / `Warning` / `Error`. `#[non_exhaustive]`.                                                            |
| `LeanPosition`                                    | `lean_rs::host::elaboration::LeanPosition`    | yes                        | `pub`        | 1-indexed `line` / `column`, optional end `line` / `column`. Mirrors Lean's `Position` shape.                                                            |
| `LEAN_HEARTBEAT_LIMIT_DEFAULT`                    | `lean_rs::host::elaboration::LEAN_HEARTBEAT_LIMIT_DEFAULT` | yes           | `pub const`  | 200_000 — matches Lean's own `maxHeartbeats` default at 4.29.1.                                                                                          |
| `LEAN_HEARTBEAT_LIMIT_MAX`                        | `lean_rs::host::elaboration::LEAN_HEARTBEAT_LIMIT_MAX`     | yes           | `pub const`  | 200_000_000 ceiling for the heartbeat setter; saturating.                                                                                                |
| `LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT`              | `lean_rs::host::elaboration::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT` | yes     | `pub const`  | 64 KiB — default cumulative diagnostic byte budget per call.                                                                                             |
| `LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`                  | `lean_rs::host::elaboration::LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`     | yes     | `pub const`  | 1 MiB ceiling for the diagnostic byte budget setter; saturating.                                                                                         |
| `LakeProject`                                     | `lean_rs::host::lake::LakeProject`          | no                           | `pub(crate)` | Lake discovery helper used by `LeanHost`.                                                                                                              |
| `LeanError`                                       | `lean_rs::error::LeanError`                 | yes                          | `pub`        | Single public error enum; `#[non_exhaustive]`. Per `RD-2026-05-17-006`, two variants: `LeanException(LeanException)` and `Host(HostFailure)`.          |
| `LeanResult<T>`                                   | `lean_rs::error::LeanResult`                | yes                          | `pub`        | `Result<T, LeanError>`. The IO-result decoder returns `LeanResult<T>` directly; there is no `IoResult<T>` alias.                                       |
| `LeanException`                                   | `lean_rs::error::LeanException`             | yes                          | `pub`        | Payload for `LeanError::LeanException`. Fields are private; accessors `kind() -> LeanExceptionKind`, `message() -> &str`.                              |
| `HostFailure`                                     | `lean_rs::error::HostFailure`               | yes                          | `pub`        | Payload for `LeanError::Host`. Fields are private; accessors `stage() -> HostStage`, `message() -> &str`.                                              |
| `HostStage`                                       | `lean_rs::error::HostStage`                 | yes                          | `pub`        | Flat `Copy` tag enum naming the host-stack stage that observed the failure (`RuntimeInit`, `Conversion`, `CallbackPanic`, `Link`, `Load`, `Internal`). |
| `LeanExceptionKind`                               | `lean_rs::error::LeanExceptionKind`         | yes                          | `pub`        | Flat `Copy` tag enum 1:1 with Lean's `IO.Error` constructors at the active toolchain version, plus `Other`.                                            |
| `LEAN_ERROR_MESSAGE_LIMIT`                        | `lean_rs::error::LEAN_ERROR_MESSAGE_LIMIT`  | yes                          | `pub const`  | 4 KiB structural bound on every `LeanError` message.                                                                                                   |

## Naming convention

- **Crate-root re-exports use the `Lean` prefix.** Disambiguates `use lean_rs::*` in mixed-language projects. Mirrors
    charter wording and existing precedent (e.g., `git2::Repository` happens to match its crate name).
- **Module-path types drop the prefix when the module path disambiguates.** `lean_rs::module::Module`,
    `lean_rs::module::Exported3`. If a power-user item is later elevated to the crate root, re-export it with the `Lean`
    prefix (e.g., `pub use crate::module::Module as LeanModule;`).
- **Internal `pub(crate)` types use lower-cased short names.** `runtime::Obj`, `runtime::ObjRef`, `abi::Except`. They
    never appear in docs.

## Lifetime cascade

The `'lean` lifetime parameter cascades through every type that holds a Lean object:

```rust
LeanRuntime              ::init() -> LeanResult<&'static LeanRuntime>
LeanHost<'lean>          ::from_lake_project(&'lean LeanRuntime, path) -> ...
LeanCapabilities<'lean, 'h>  ::load_capabilities(&'h LeanHost<'lean>, package, lib_name)
LeanSession<'lean, 'c>   ::session(&'c LeanCapabilities<'lean, '_>, imports)
LeanExpr<'lean>          // (and the other handles)
```

The `'lean` parameter is invisible at a typical call site (inferred from the runtime borrow). Compile-time enforcement:
no handle can outlive the runtime borrow, no `Obj` can be constructed before `init()` returns, no value escapes to
another thread (all types are `!Send + !Sync` by default).

## Methods on the curated types (happy-path shape)

The methods named here exist on the curated types by prompt 18's verification. Earlier prompts may introduce them
piecewise. Doc comments and `# Errors` / `# Panics` sections are mandatory.

- `LeanModule::exported<Args, R>(&self, name: &str) -> LeanResult<LeanExported<'lean, 'lib, Args, R>>` where
    `Args: LeanArgs<'lean>` and `R: DecodeCallResult<'lean>`. Per `RD-2026-05-17-007`.
- `LeanExported::<(A1, ..., AN), R>::call(&self, a1: A1, ..., aN: AN) -> LeanResult<R::Output>` (macro-stamped for
    arity 0..=12). The fn-ptr cast is per-arg `<Ai as LeanAbi>::CRepr → R::CRepr`; for `R = LeanIo<T>`, the return is
    routed through `decode_io` + `T::try_from_lean`.
- `LeanHost::from_lake_project(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self>`
- `LeanHost::load_capabilities(&self, package: &str, lib_name: &str) -> LeanResult<LeanCapabilities<'lean, '_>>`
    (two-arg because Lake's compiled dylib basename is `lib{escaped_package}_{lib_name}.{dylib,so}` with `_` →
    `__` escaping on the package; both pieces are required to resolve the on-disk artifact)
- `LeanCapabilities::session(&self, imports: &[&str]) -> LeanResult<LeanSession<'lean, '_>>`
- `LeanSession::query_declaration(&mut self, name: &str) -> LeanResult<LeanDeclaration<'lean>>`
- `LeanSession::elaborate(&mut self, source: &str, expected_type: Option<&LeanExpr<'lean>>, options: &LeanElabOptions) -> LeanResult<Result<LeanExpr<'lean>, LeanElabFailure>>`
- `LeanSession::kernel_check(&mut self, source: &str, options: &LeanElabOptions) -> LeanResult<LeanKernelOutcome<'lean>>`
- `LeanSession::check_evidence(&mut self, handle: &LeanEvidence<'lean>) -> LeanResult<EvidenceStatus>` (prompt 17)
- `LeanSession::summarize_evidence(&mut self, handle: &LeanEvidence<'lean>) -> LeanResult<ProofSummary>` (prompt 17)
- `LeanSession::run_meta<Req, Resp>(&mut self, service: &LeanMetaService<Req, Resp>, request: Req, options: &LeanMetaOptions) -> LeanResult<LeanMetaResponse<Resp>>`
    where `Req: LeanAbi<'lean>` and `Resp: TryFromLean<'lean>`. The `LeanMetaService`, `LeanMetaResponse`,
    `LeanMetaOptions`, `LeanMetaTransparency`, `MetaCallStatus`, and the three service constructors `infer_type` /
    `whnf` / `heartbeat_burn` live at `lean_rs::host::meta::*` — see *Specialized sub-module surfaces*.
- `LeanSession::query_declarations_bulk(&mut self, names: &[&str]) -> LeanResult<Vec<LeanDeclaration<'lean>>>` (prompt
    20). Strict semantics — the first missing name errors the batch with `Host(Conversion)` naming it. Costs `N + 1`
    FFI calls (one bulk dispatch + `N` `name_from_string`) vs. `2 * N` for the singular fold.
- `LeanSession::elaborate_bulk(&mut self, sources: &[&str], options: &LeanElabOptions) -> LeanResult<Vec<Result<LeanExpr<'lean>, LeanElabFailure>>>`
    (prompt 20). Per-source `Result` mirrors `LeanSession::elaborate` exactly. No `expected_type` parameter — deferred
    until a real second caller earns the per-source `&[Option<&LeanExpr>]` surface.
- `LeanSession::call_capability<Args, R>(&mut self, name: &str, args: Args) -> LeanResult<R::Output>` where
    `Args: LeanArgs<'lean>` and `R: DecodeCallResult<'lean>` (prompt 20). Function-only escape hatch for invoking
    capability-dylib exports beyond the thirteen session-fixed symbols; reuses the same `Args` / `R` bounds as
    `LeanModule::exported`, including the `LeanIo<T>` IO marker.
- `LeanSession::stats(&self) -> SessionStats` — snapshot of per-session dispatch metrics.
- `SessionPool::with_capacity(runtime: &'lean LeanRuntime, capacity: usize) -> Self` (prompt 20)
- `SessionPool::acquire<'p, 'c>(&'p self, caps: &'c LeanCapabilities<'lean, 'c>, imports: &[&str]) -> LeanResult<PooledSession<'lean, 'p, 'c>>`
    (prompt 20). Capability-agnostic storage — entries are bare `Obj<'lean>` environments rewrapped under the
    supplied capability borrow at acquire time. FIFO on take, LIFO on push; matches the `imports`-list cache key
    structurally.
- `SessionPool::stats(&self) -> PoolStats`, `len()`, `is_empty()`, `capacity()` — observability.

None leak raw `lean_*` types, raw refcount obligations, or initializer-symbol order.

## Error model

`LeanError` is the only public error type that crosses the boundary. Per `RD-2026-05-17-006`, it has two variants:
`LeanException(LeanException)` for Lean-thrown `IO` errors that callers may surface to end users, and
`Host(HostFailure)` for any failure of the host stack (init, link, load, conversion, contained callback panic, internal
invariant). The payload structs have private fields and `pub(crate)` constructors that run the bounding helper, so the
`LEAN_ERROR_MESSAGE_LIMIT` (4 KiB) cap on `message()` is a structural invariant rather than convention — external
callers receive `LeanError` values but cannot mint one with an unbounded message. The `stage` and `kind` flat enums are
diagnostic tags; callers rarely match on them and instead read `message()`.

`Except<E, T>` is a **value type**, not an error. When an exported function returns `IO (Except E T)`:

1. Outer `IO` failure → `LeanError::LeanException` (host failure).
1. Inner `Except` decodes via `TryFromLean` into Rust `Result<T, E>` (application semantics).

The caller sees `LeanResult<Result<T, E>>` and decides how to flatten. The rule: **runtime / host failures are
`LeanError`; application semantics are values.**

## Verification (forward)

The classification table is satisfied when, after prompt 18:

1. `rg -n "^pub use" crates/lean-rs/src/lib.rs` matches exactly the curated set above.
1. The prompt 18 end-to-end integration test uses only `use lean_rs::*` items (no module-path access).
1. A compile-fail test confirms a handle cannot outlive the runtime borrow.
1. A compile-fail test confirms `LeanRuntime`, `LeanSession`, and the handles are neither `Send` nor `Sync`.
1. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` is clean and every curated item has a doc comment.
