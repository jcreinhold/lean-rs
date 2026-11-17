# Same-Process Controls

`lean-rs-host` bounds cooperative work, output, caches, and a closed metadata query. Worker children provide the process
reset boundary for import-heavy or potentially wedged work; see [production hosting](production-hosting.md).

| Control | Public surface | Boundary |
| --- | --- | --- |
| Heartbeats | `LeanElabOptions`, `LeanMetaOptions` | Operations that check `Lean.maxHeartbeats`; no hard preemption. |
| Cancellation | `LeanCancellationToken` | Rust dispatch boundaries and cancellable bulk items; no interruption of an in-flight Lean call. |
| Diagnostic bytes | Elaboration/meta `diagnostic_byte_limit` | Returned diagnostic collection, rather than all Lean allocations. |
| Render/output bytes | `ModuleQueryOutputBudgets`, `DeclarationInspectionBudgets` | Projected module-query, proof-attempt, verification, and inspection output. |
| Search rows | `DeclarationSearchRequest::limit` | Result count; Lean also clamps broad requests internally. |
| Snapshot cache clearing | `LeanSession::clear_module_snapshot_cache` | Shim-owned snapshots, rather than runtime reset or module unloading. |
| Import facts | `LeanSession::import_stats`, `LeanImportStats::memory_diagnostic` | Attribution for compacted regions, extensions, and bytes. |
| Import admission | `LeanSessionImportProfile`, `SessionPoolMemoryPolicy` | Which full-session imports enter Lean and how they are described. |
| Bracketed import | `LeanCapabilities::bracketed_import_query` | One closed declaration-metadata query without loaded extensions. |

## Import lifetime

Full sessions use `loadExts := true` for parser, elaborator, proof-state, pretty-printer, source-range, and capability
workflows. Extension state may retain references into compacted regions. Neither session drop nor cache clearing makes
`Environment.freeRegions` safe on that path.

The bracketed query uses `withImportModules` with `loadExts := false`, serializes declaration metadata to JSON, and
frees regions before Rust parses the owned result. No environment, expression, name, declaration, extension state,
session, or capability handle may escape. It cannot support normal elaboration, parser-backed queries, or
pretty-printing, and is not a full-session import profile. A new bracketed query returning Lean-owned objects needs a
local lifetime proof before it can be safe.

Same-process controls cannot recover from native abort, process exit, foreign unwind, or a wedged Lean runtime. The
[memory evidence](safety/long-session-memory.md) records local measurements and the retained-state model.

## Verify changes

Test defaults and FFI normalization without full imports where possible. Use focused host tests for heartbeat
exhaustion, diagnostic truncation, and pre-cancelled tokens. The bracketed-import test must check `load_exts == false`,
`free_regions_ran == true`, and owned metadata. Run integration tests through `cargo nextest`; see
[testing](testing.md).
