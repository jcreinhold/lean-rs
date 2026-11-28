# Standard Lean Service Layer

`lean-rs-host` provides imports, declaration inspection and search, elaboration, kernel-checked evidence, bounded
`MetaM`, module-query projections, proof attempts, cancellation, progress, and session reuse. It builds on the typed FFI
in `lean-rs` and forbids unsafe Rust. Applications that only call explicit Lean `@[export]` functions can use `lean-rs`
directly.

The public API lives in the
[crate root](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/lib.rs), public `host` modules, and
[API baseline](../api-review/lean-rs-host-public.txt). This note records the behavioral boundaries rather than
maintaining a second list of Rust signatures.

## Construction and lifetime

```rust
let runtime = lean_rs::LeanRuntime::init()?;
let host = lean_rs_host::LeanHost::from_lake_project(runtime, lake_root)?;
let capabilities = host.load_shims_only()?;
let mut session = capabilities.session(&["MyProject"], None, None)?;
let declaration = session.query_declaration("MyProject.example", None)?;
```

`from_lake_project` validates the root directory. `load_shims_only` prepares the bundled host and generic interop
libraries without opening a user capability dylib. `load_capabilities(package, lib_name)` also opens and initializes
that capability's shared library. Lake reports the library's file for `lib_name`, and the initializer symbol needs both
names.

`LeanHost<'lean>` borrows the runtime. `LeanCapabilities<'lean, 'h>` borrows the host, and `LeanSession<'lean, 'c>`
borrows capabilities while owning its imported environment. Semantic handles from `lean-rs` and `LeanEvidence<'lean>`
retain the runtime lifetime. `PooledSession` additionally borrows its pool. The
[compile-fail fixtures](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs-host/tests/compile_fail) pin
these lifetimes and the `!Send + !Sync` contract. All Lean work stays on the thread where the handles were constructed.

## Service boundaries

| Surface | Responsibility |
| --- | --- |
| Crate-root `LeanHost`, `LeanCapabilities`, `LeanSession` | Project binding, prepared capabilities, imports, and session dispatch |
| Crate-root declaration request/result types | Bounded inspection, search, verification, and proof-attempt data |
| `lean_rs_host::host::elaboration` and root re-exports | `LeanElabOptions`, diagnostics, and elaboration failures |
| `lean_rs_host::host::evidence` and root re-exports | Opaque checked evidence, outcome tags, and display summaries |
| `lean_rs_host::meta` | Opt-in bounded `MetaM` services |
| `lean_rs_host::host::process` | Module queries, selector batches, snapshot-cache policy, and proof-workflow projections |
| Crate-root `SessionPool`, `PooledSession`, configuration and statistics | Per-thread environment reuse and memory-retention policy |

`LeanName`, `LeanLevel`, `LeanExpr`, `LeanDeclaration`, the runtime, and `LeanError` belong to `lean-rs`. Host methods
reuse these types without re-exporting a parallel handle model. Low-level export dispatch and callback mechanisms remain
there; host methods expose named services rather than arbitrary symbol calls.

The host owns and packages its Lean shims. Sessions resolve mandatory bindings once; unavailable optional operations
return their documented `Unsupported` outcome. The exact export rows and ABI shapes are defined in
[`host_shim_abi.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-toolchain/src/host_shim_abi.rs) and
described in the [host capability contract](../lean-rs-host-capability-contract.md).

## Checked host ABI metadata

`lean_toolchain::host_shim_exports!` supplies the same symbol and Rust argument/return tokens to the host binding
generator and an ABI-only projection. The projection uses private marker types and needs no Lean runtime or loaded
manifest. An all-row host test independently computes `LeanArgs` / `DecodeCallResult` signatures and compares them with
that projection. An unknown shape requires an explicit marker implementation.

`host_shim_abi::expected_checked_bindings()` selects required proof-workflow bindings by generated field access.
Compiled expectations and loaded receipts use the same canonical JSON/SHA encoder, including recursive key sorting. The
worker parent can check those receipts at startup and replacement without linking `libleanshared`. Runtime and header
checks remain separate. Only declaration metadata moves below the host layer; operations and semantic types remain in
`lean-rs-host`.

## Query and bulk semantics

The [session implementation](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/host/session.rs)
owns dispatch accounting and the following bulk behavior:

| Method | Missing declaration or failed item |
| --- | --- |
| `query_declarations_bulk` | First missing name fails the entire batch with a conversion error naming it |
| `declaration_type_bulk` | `None` at the corresponding input position |
| `declaration_kind_bulk` | `"missing"` at the corresponding position |
| `declaration_name_bulk` | The input name at the corresponding position |
| `elaborate_bulk` | Per-source `Result<LeanExpr, LeanElabFailure>`; budgets apply separately to each source |

Without a cancellation token, bulk operations dispatch the batch in Lean. `query_declarations_bulk` first constructs one
name per input, so its recorded FFI count is `N + 1`, compared with `2N` singular calls. See
[same-process controls](../same-process-controls.md) for cancellation, progress, batching changes, and their limits.

`declaration_source_range` returns Lean's recorded 1-based range, with a resolved source path where available and a
module-label fallback. Filtered declaration listing folds the environment in Lean before allocating returned Rust
handles. Module queries and proof workflows return bounded Lean-authored projections; they do not expose raw info trees.
Their cache, options, and degraded-result rules live in [`module-queries.md`](module-queries.md).

## Evidence and resource bounds

`kernel_check` returns `LeanKernelOutcome`: checked evidence or diagnostics classified as rejected, unavailable, or
unsupported. `LeanEvidence` remains opaque; `check_evidence` asks Lean to revalidate it, and `summarize_evidence`
produces bounded display strings. A `ProofSummary` is diagnostic data, not a proof certificate outside its producing
session. `LeanKernelOutcome` contains evidence and is therefore thread-bound; its `EvidenceStatus` projection is plain
Rust data.

The current bounds are defined in
[shared metadata](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-toolchain/src/lib.rs) and
[proof summaries](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/host/evidence/summary.rs):

| Limit | Default | Ceiling |
| --- | ---: | ---: |
| Elaboration heartbeats | 200,000 | 200,000,000 |
| Cumulative diagnostics per call | 64 KiB | 1 MiB |
| Each `ProofSummary` string | 4 KiB | 4 KiB |

Option setters saturate at their ceilings. Heartbeats bound Lean work that consumes them; they are not a wall-clock
watchdog. `LeanElabOptions::lean_options` supplies typed project options for source processing, with the host heartbeat
limit taking precedence over `maxHeartbeats`.

## Pooling

`SessionPool` caches environment references by canonical project root, ordered imports, import profile, and extension
registry safety state. Import order is significant. The pool rewraps reusable environments under the supplied capability
borrow rather than retaining old capability references. Acquiring and returning sessions is single-threaded.

Capacity bounds cached entries; `SessionPoolMemoryPolicy` can additionally refuse a fresh cache-miss import after an
import-count or RSS admission limit is reached. It does not impose an RSS-based release policy on warm entries. Pool
statistics distinguish reuse, key misses, releases, and drains. `drain()` releases cached Rust-owned environment
references and returns the number removed. Checked-out sessions stay valid and may return later. Draining does not reset
Lean's process-global import/runtime state or promise reclaimed RSS; see
[long-session memory](../safety/long-session-memory.md).

## Errors and implementation boundary

`LeanError` is shared by the typed FFI and host layers: `LeanException` for Lean `IO` errors, `Host` for host failures,
and `Cancelled` for observed cancellation. Messages are bounded to `LEAN_ERROR_MESSAGE_LIMIT` (4 KiB). An exported
`IO (Except E T)` decodes to `LeanResult<Result<T, E>>`: outer runtime failures are errors, inner application outcomes
are values.

The host uses `lean_rs::__host_internals` for bounded error construction and other sibling support. Host-defined ABI
records use the hidden `SealedAbi` extension convention. These hidden public paths support the sibling crate; they are
not a general downstream ABI extension API. In particular, a public hidden marker is not a Rust visibility barrier.

New service APIs must preserve these boundaries. Keep specialised operations at explicit module paths, curate root
re-exports, and update the API baseline with intentional public changes. Verify the curated surface, lifetime fixtures,
and rustdoc checks using the commands in [testing](../testing.md).
