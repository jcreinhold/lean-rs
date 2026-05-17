# Host API surface

This document pins the curated public API of `lean-rs`: which items cross the crate-root boundary, which stay at module
paths, and which stay `pub(crate)`. The table is the **semver surface** for the published crate. Refactors that reshape
internal modules are free as long as the curated re-exports below stay stable.

The classification is set in advance by `RD-2026-05-17-004` (see also [`04-raw-sys-design.md`](04-raw-sys-design.md) for
the sibling rationale on `lean-rs-sys`) so that prompt 18 has a table to verify against, not to invent. Implementation
prompts 06–17 must land items consistent with this table.

## Layering recap

`lean-rs-sys` → `lean-toolchain` → `lean-rs`. Inside `lean-rs`:

- `error` (pub): `LeanError`, `LeanResult`, sub-error structs.
- `module` (pub): typed exported-function loading.
- `host` (pub): semantic sessions, capabilities, opaque handles, evidence.
- `runtime` (pub(crate)): init, `Obj<'lean>`, thread guards.
- `abi` (pub(crate)): scalar / string / array / option / except conversions.

Batch and session-pool operations are methods on `LeanSession`. There is no sibling `batch` module.

## Curated crate-root surface

```rust
// crates/lean-rs/src/lib.rs (illustrative — landed by prompt 18)
pub use crate::error::{
    LeanError, LeanResult,
    LeanException, HostFailure,
    HostStage, LeanExceptionKind,
    LEAN_ERROR_MESSAGE_LIMIT,
};
pub use crate::host::{LeanHost, LeanCapabilities, LeanSession};
pub use crate::host::handle::{LeanName, LeanLevel, LeanExpr, LeanDeclaration};
pub use crate::host::evidence::{LeanEvidence, ProofSummary, EvidenceStatus};
pub use crate::runtime::LeanRuntime;
```

`LeanModule` and the `LeanExported{N}` family stay at `lean_rs::module::*` for power users; they are not at the crate
root because the happy path goes through `LeanHost` → `LeanCapabilities` → `LeanSession`.

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
| `LeanExported0<'lean, R>` … `LeanExported12<...>` | `lean_rs::module::LeanExported{N}`          | no                           | `pub`        | Macro-stamped typed function handles for arities 0..=12.                                                                                               |
| `LeanHost<'lean>`                                 | `lean_rs::host::LeanHost`                   | yes                          | `pub`        | Entry point. `from_lake_project(runtime, path)`.                                                                                                       |
| `LeanCapabilities<'lean, 'h>`                     | `lean_rs::host::LeanCapabilities`           | yes                          | `pub`        | Loaded capability module reference.                                                                                                                    |
| `LeanSession<'lean, 'c>`                          | `lean_rs::host::LeanSession`                | yes                          | `pub`        | Long-lived imports + queries; **owns batch/bulk methods**.                                                                                             |
| `LeanName<'lean>`                                 | `lean_rs::host::handle::LeanName`           | yes                          | `pub`        | Opaque semantic handle.                                                                                                                                |
| `LeanLevel<'lean>`                                | `lean_rs::host::handle::LeanLevel`          | yes                          | `pub`        | Opaque semantic handle.                                                                                                                                |
| `LeanExpr<'lean>`                                 | `lean_rs::host::handle::LeanExpr`           | yes                          | `pub`        | Opaque semantic handle.                                                                                                                                |
| `LeanDeclaration<'lean>`                          | `lean_rs::host::handle::LeanDeclaration`    | yes                          | `pub`        | Opaque semantic handle.                                                                                                                                |
| `LeanEvidence<'lean>`                             | `lean_rs::host::evidence::LeanEvidence`     | yes                          | `pub`        | Opaque checked-evidence handle.                                                                                                                        |
| `ProofSummary`                                    | `lean_rs::host::evidence::ProofSummary`     | yes                          | `pub`        | Lean-authored display + status; not trusted outside the session.                                                                                       |
| `EvidenceStatus`                                  | `lean_rs::host::evidence::EvidenceStatus`   | yes                          | `pub`        | Tag enum: `Checked` / `Rejected` / `Unavailable` / `Unsupported`.                                                                                      |
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
LeanCapabilities<'lean, 'h>  ::load_capabilities(&'h LeanHost<'lean>, name)
LeanSession<'lean, 'c>   ::session(&'c LeanCapabilities<'lean, '_>, imports)
LeanExpr<'lean>          // (and the other handles)
```

The `'lean` parameter is invisible at a typical call site (inferred from the runtime borrow). Compile-time enforcement:
no handle can outlive the runtime borrow, no `Obj` can be constructed before `init()` returns, no value escapes to
another thread (all types are `!Send + !Sync` by default).

## Methods on the curated types (happy-path shape)

The methods named here exist on the curated types by prompt 18's verification. Earlier prompts may introduce them
piecewise. Doc comments and `# Errors` / `# Panics` sections are mandatory.

- `LeanHost::from_lake_project(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self>`
- `LeanHost::load_capabilities(&self, module_name: &str) -> LeanResult<LeanCapabilities<'lean, '_>>`
- `LeanCapabilities::session(&self, imports: &[&str]) -> LeanResult<LeanSession<'lean, '_>>`
- `LeanSession::query_declaration(&mut self, name: &str) -> LeanResult<LeanDeclaration<'lean>>`
- `LeanSession::elaborate(&mut self, source: &str) -> LeanResult<LeanExpr<'lean>>`
- `LeanSession::check_evidence(&mut self, handle: &LeanEvidence<'lean>) -> LeanResult<EvidenceStatus>`
- `LeanSession::query_declarations_bulk(&mut self, names: &[&str]) -> LeanResult<Vec<LeanDeclaration<'lean>>>` (prompt
    20\)
- `LeanSession::with_session_pool(...) -> ...` (prompt 20 — exact signature deferred)

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
