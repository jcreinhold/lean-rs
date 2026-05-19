# `lean-rs-host` Capability Contract

The 18 mandatory + 4 optional `@[export] lean_rs_host_*` symbols
[`lean-rs-host`](https://docs.rs/lean-rs-host)'s `LeanCapabilities::load_capabilities`
resolves at runtime. The shim package
[`lean-rs-host-shims`](https://github.com/jcreinhold/lean-rs/tree/main/lake/lean-rs-host-shims)
ships an implementation; external consumers add `require lean_rs_host_shims from "…"` to
their `lakefile.lean`. Expected on-disk dylib name:
`liblean__rs__host__shims_LeanRsHostShims.{dylib,so}`.

This document covers the **wire-level contract**: each Lean signature, the typed Rust shape
`LeanSession::*` exposes on top, and the Rust call-site mapping. The architectural rationale
(two dylibs, `RTLD_GLOBAL`, the consumer `lakefile.lean` shape) lives in
[`architecture/04-host-stack.md`](architecture/04-host-stack.md).

## Resolution

`LeanCapabilities::new` performs:

1. `LakeProject::shim_dylib()` reads the consumer's `lake-manifest.json` for the entry whose `name` is `lean_rs_host_shims`. Handles `type: "path"` (`<dir>` resolved relative to the project root) and `type: "git"` (`.lake/packages/lean_rs_host_shims/`).
2. `LeanLibrary::open_globally(runtime, shim_dylib_path)` opens the shim dylib first, with `RTLD_LAZY | RTLD_GLOBAL` on Unix so the subsequently opened consumer dylib can resolve its transitive reference to `_initialize_lean__rs__host__shims_LeanRsHostShims`.
3. `shim_library.initialize_module("lean_rs_host_shims", "LeanRsHostShims")` runs the shim's root initializer, which transitively initializes `Lean.*` (idempotent via Lean's `_G_initialized` flag).
4. `user_library.initialize_module(<package>, <lib_name>)` runs the consumer's root initializer; it reaches the now-global shim symbols through the dynamic linker.
5. `SessionSymbols::resolve(&shim_library)` populates every `lean_rs_host_*` address from the shim dylib. The consumer dylib's `LeanSession::call_capability` route stays open for user-authored `@[export]` symbols.

A missing mandatory symbol fails capability load with `LeanError::Host(stage = Link)`. A
missing optional meta-service symbol stores `None` in `SessionSymbols`;
`LeanSession::run_meta` returns `LeanMetaResponse::Unsupported` for that service at dispatch
time.

## Mandatory contract (18 symbols)

Lean structure types (`ElabOpts`, `ElabResult`, `Evidence`, `EvidenceStatus`,
`KernelOutcome`, `ProofSummary`, `MetaOpts`, `MetaResponse`, `DeclarationFilter`,
`SourceRange`) live in
[`lake/lean-rs-host-shims/LeanRsHostShims/Elaboration.lean`](../lake/lean-rs-host-shims/LeanRsHostShims/Elaboration.lean)
and [`Meta.lean`](../lake/lean-rs-host-shims/LeanRsHostShims/Meta.lean). Rust counterparts and
the `TryFromLean` / `IntoLean` impls crossing the ABI live in
`crates/lean-rs-host/src/host/{elaboration,evidence,meta}/`.
`DeclarationFilter` is a private wire record whose three flags are Nat-backed
`0`/`1` values so it uses the same object-slot structure ABI as the rest of the
host-defined records; Rust callers see ordinary `bool` fields.

### Environment and declaration queries (13)

| Lean symbol | Lean signature | Rust method on `LeanSession` |
| --- | --- | --- |
| `lean_rs_host_session_import` | `(searchPaths : Array String) (importNames : Array String) : IO Environment` | called once by `LeanCapabilities::session(imports, cancellation)` |
| `lean_rs_host_name_from_string` | `(s : String) : Name` | internal helper for every name-bearing query |
| `lean_rs_host_env_query_declaration` | `(env : Environment) (name : Name) : IO (Option Declaration)` | `query_declaration(name, cancellation)` |
| `lean_rs_host_env_query_declarations_bulk` | `(env : Environment) (names : Array Name) : IO (Array (Option Declaration))` | `query_declarations_bulk(names, cancellation)` |
| `lean_rs_host_env_list_declarations` | `(env : Environment) : IO (Array Name)` | `list_declarations(cancellation)` |
| `lean_rs_host_env_list_declarations_filtered` | `(env : Environment) (filter : DeclarationFilter) : IO (Array Name)` | `list_declarations_filtered(filter, cancellation)` |
| `lean_rs_host_env_declaration_source_range` | `(env : Environment) (name : Name) (sourceRoots : Array String) : IO (Option SourceRange)` | `declaration_source_range(name, cancellation)` |
| `lean_rs_host_env_declaration_type` | `(env : Environment) (name : Name) : IO (Option Expr)` | `declaration_type(name, cancellation)` |
| `lean_rs_host_env_declaration_type_bulk` | `(env : Environment) (names : Array String) : IO (Array (Option Expr))` | `declaration_type_bulk(names, cancellation)` |
| `lean_rs_host_env_declaration_kind` | `(env : Environment) (name : Name) : IO String` | `declaration_kind(name, cancellation)` |
| `lean_rs_host_env_declaration_kind_bulk` | `(env : Environment) (names : Array String) : IO (Array String)` | `declaration_kind_bulk(names, cancellation)` |
| `lean_rs_host_env_declaration_name` | `(_env : Environment) (name : Name) : IO String` | `declaration_name(name, cancellation)` |
| `lean_rs_host_env_declaration_name_bulk` | `(_env : Environment) (names : Array String) : IO (Array String)` | `declaration_name_bulk(names, cancellation)` |

### Elaboration, kernel check, evidence (5)

| Lean symbol | Lean signature | Rust method on `LeanSession` |
| --- | --- | --- |
| `lean_rs_host_elaborate` | `(env) (src : String) (expectedType : Option Expr) (opts : ElabOpts) : IO ElabResult` | `elaborate(source, expected_type, options, cancellation)` |
| `lean_rs_host_elaborate_bulk` | `(env) (sources : Array String) (opts : ElabOpts) : IO (Array ElabResult)` | `elaborate_bulk(sources, options, cancellation)` |
| `lean_rs_host_kernel_check` | `(env) (src : String) (opts : ElabOpts) : IO KernelOutcome` | `kernel_check(source, options, cancellation)` |
| `lean_rs_host_check_evidence` | `(env) (ev : Evidence) : IO EvidenceStatus` | `check_evidence(evidence, cancellation)` |
| `lean_rs_host_evidence_summary` | `(_env) (ev : Evidence) : IO ProofSummary` | `summarize_evidence(evidence, cancellation)` |

## Optional contract (4 symbols—bounded `MetaM`)

If absent at load time, `SessionSymbols::resolve_optional_function_symbol` stores `None` for
that slot; `LeanSession::run_meta` synthesises `LeanMetaResponse::Unsupported` for any
service mapped to the missing address.

| Lean symbol | Lean signature | Rust method on `LeanSession` |
| --- | --- | --- |
| `lean_rs_host_meta_infer_type` | `(env) (expr : Expr) (opts : MetaOpts) : IO MetaResponse` | `run_meta(&meta::infer_type(), expr, options, cancellation)` |
| `lean_rs_host_meta_whnf` | `(env) (expr : Expr) (opts : MetaOpts) : IO MetaResponse` | `run_meta(&meta::whnf(), expr, options, cancellation)` |
| `lean_rs_host_meta_heartbeat_burn` | `(env) (_expr : Expr) (opts : MetaOpts) : IO MetaResponse` | `run_meta(&meta::heartbeat_burn(), expr, options, cancellation)` |
| `lean_rs_host_meta_is_def_eq` | `(env) (request : Expr × Expr × UInt8) (opts : MetaOpts) : IO MetaResponse` | `run_meta(&meta::is_def_eq(), (lhs, rhs, transparency), options, cancellation)` |

## Forking the shim package

The shim package is small (~557 LOC across three files). A fork that customises behaviour
(e.g., different heartbeat policy, extra logging on the kernel-check path) must keep:

- Same Lake package name (`lean_rs_host_shims`) and `lean_lib` name (`LeanRsHostShims`) so `LeanCapabilities` finds the dylib at the conventional path.
- Same 18 mandatory `@[export]` symbol names with compatible signatures (the Rust side casts function pointers to fixed shapes).
- The 4 optional meta-service symbols are truly optional; omitting any collapses the corresponding `run_meta` service to `Unsupported`.

A fork that changes the Lean structure layouts also needs corresponding Rust changes—this
is why the shim package isn't framed as "compatibility shims" but as the **implementation** of
the wire contract.
