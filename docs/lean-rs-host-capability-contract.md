# Host Shim Contract

`lean-rs-host` bundles the Lean implementation of its session services under
[`shims/lean-rs-host-shims/`](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs-host/shims/lean-rs-host-shims). Consumers do not add a
`lean_rs_host_shims` dependency to their Lake package. The host loads the shim capability and its generic interop
dependency separately from the consumer dylib.

The authoritative export table is [`host_shim_exports!`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-toolchain/src/host_shim_abi.rs). It declares 34
mandatory and 17 optional exports, including their Rust argument and result types. That one table generates manifest
signatures, cached typed bindings, and link-free startup expectations. Tests in
[`shim_bindings.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/host/shim_bindings.rs) compare the generated ABI projection with the
real typed signatures and the packaged Lean exports. Change the table, Lean implementation, and Rust decoding together.

## Loading and resolution

`LeanHost::load_capabilities` prepares the bundled artifacts, loads their manifest-backed capability, and initializes
the consumer's root module. `load_shims_only` omits the consumer dylib. Prepared-artifact entry points load an existing
snapshot without running a build. Session import uses search paths for the consumer project and both shim packages. See
[the host architecture](architecture/host-services.md) for ownership and loading rationale.

Each session resolves `HostShimBindings` once. A missing mandatory symbol fails construction. A missing optional symbol
leaves an empty binding; its operation reports unsupported or uses its documented fallback. Other checked-lookup errors,
including signature mismatches, fail construction even for optional exports. Arbitrary consumer `@[export]` dispatch
belongs to the lower-level `lean-rs` API.

## Mandatory exports

Every symbol below has the prefix `lean_rs_host_`. Argument and result layouts come from the authoritative table and the
linked Lean definitions; the names here map the contract to its callers.

### Imports and names

Defined in [Environment.lean](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/shims/lean-rs-host-shims/LeanRsHostShims/Environment.lean).

| Export suffix | Rust operation |
| --- | --- |
| `session_import` | Default session import |
| `session_import_progress` | Default import with a progress sink |
| `session_import_profile` | Explicit import profile or profiler options |
| `session_import_profile_progress` | Explicit profile with progress |
| `env_import_stats` | `LeanSession::import_stats` |
| `extension_registry_epoch` | Captured/live extension-registry epoch queries |
| `bracketed_import_query` | `LeanCapabilities::bracketed_import_query` |
| `name_from_string` | Internal conversion for name-bearing queries |
| `name_to_string` | `name_to_string`, `name_to_string_bulk`, `list_declarations_strings` |

Full sessions load extensions. The bracketed query imports without extensions, returns serialized owned metadata, and
frees regions before returning. Its lifetime restriction is described in
[same-process controls](same-process-controls.md). `ImportStats.importedBytes` is a compatibility alias for total
compacted-region bytes; separate fields report mapped and non-mapped regions. The registry epoch is opaque and monotone;
callers compare epochs for equality.

### Declaration queries

Also defined in [Environment.lean](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/shims/lean-rs-host-shims/LeanRsHostShims/Environment.lean).

| Export suffix | `LeanSession` operation |
| --- | --- |
| `env_query_declaration` | `query_declaration` |
| `env_query_declarations_bulk` | `query_declarations_bulk` |
| `env_query_declarations_bulk_progress` | Bulk query with progress |
| `env_list_declarations` | `list_declarations` |
| `env_list_declarations_filtered` | `list_declarations_filtered` |
| `env_list_declarations_filtered_progress` | Filtered list with progress |
| `env_declaration_source_range` | `declaration_source_range` |
| `env_declaration_type` | `declaration_type` |
| `env_declaration_type_bulk` | `declaration_type_bulk` |
| `env_declaration_type_bulk_progress` | Bulk type query with progress |
| `env_declaration_kind` | `declaration_kind` |
| `env_declaration_kind_bulk` | `declaration_kind_bulk` |
| `env_declaration_kind_bulk_progress` | Bulk kind query with progress |
| `env_declaration_name` | `declaration_name` |
| `env_declaration_name_bulk` | `declaration_name_bulk` |
| `env_declaration_name_bulk_progress` | Bulk name query with progress |
| `env_search_declarations` | `search_declarations` |
| `env_expr_to_string_raw` | `expr_to_string_raw` |

`DeclarationFilter` uses Nat-backed `0`/`1` flags in its private wire record; callers see Rust `bool` fields.

### Elaboration and evidence

Defined in [Elaboration.lean](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/shims/lean-rs-host-shims/LeanRsHostShims/Elaboration.lean).

| Export suffix | `LeanSession` operation |
| --- | --- |
| `elaborate` | `elaborate` |
| `elaborate_bulk` | `elaborate_bulk` |
| `elaborate_bulk_progress` | Bulk elaboration with progress |
| `kernel_check` | `kernel_check` |
| `kernel_check_progress` | Kernel check with progress |
| `check_evidence` | `check_evidence` |
| `evidence_summary` | `summarize_evidence` |

Options are passed through the scalar/string argument shapes in the export table, rather than as a single C-ABI options
record. Lean-authored result structures are decoded by the Rust elaboration and evidence modules.

## Optional exports

The five meta services are defined in
[Meta.lean](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/shims/lean-rs-host-shims/LeanRsHostShims/Meta.lean); inspection and runtime version
are in `Environment.lean`; module processing and caches are in
[InfoTree.lean](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/shims/lean-rs-host-shims/LeanRsHostShims/InfoTree.lean).

| Export suffix | Rust operation |
| --- | --- |
| `meta_infer_type` | `run_meta(&meta::infer_type(), ...)` |
| `meta_whnf` | `run_meta(&meta::whnf(), ...)` |
| `meta_heartbeat_burn` | `run_meta(&meta::heartbeat_burn(), ...)` |
| `meta_is_def_eq` | `run_meta(&meta::is_def_eq(), ...)` |
| `meta_pp_expr` | `run_meta(&meta::pp_expr(), ...)` |
| `env_inspect_declaration` | `inspect_declaration` |
| `process_module_query` | `process_module_query` |
| `process_module_query_batch` | `process_module_query_batch` |
| `process_module_query_batch_cached` | `process_module_query_batch_cached` |
| `attempt_proof` | `attempt_proof` |
| `verify_declaration` | `verify_declaration` |
| `verify_declaration_batch` | `verify_declaration_batch` |
| `configure_header_import_budget` | Worker header-import budget setup |
| `header_import_state` | Worker header-import accounting |
| `clear_module_header_cache` | Header cache cleanup at session release |
| `runtime_version` | Loaded Lean runtime version check |
| `clear_module_snapshot_cache` | `clear_module_snapshot_cache` |

Inspection results include `proofSearch` availability and `derivedWork` attribution. Requesting attributes alone does
not compute proof-search facts; `fields.proofSearch` defaults off. Module proof-state results include bounded,
source-ordered `proofBoundaries` recovery points and an explicit truncation flag, including selector-failure results.

## Custom implementations

A replacement shim package must preserve package/library names, mandatory exports, manifest-compatible signatures, and
all structure layouts Rust decodes. Optional exports can be omitted only with their caller's unsupported/fallback
behavior understood. Layout changes require matching Rust changes. Keep reusable callback and ABI helpers in
`lean-rs-interop-shims`; host policy belongs here.
