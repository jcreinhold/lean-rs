# Same-Process Controls

`lean-rs-host` exposes scoped controls for trusted same-process Lean work. These controls bound cooperative work,
host-side materialization, cache growth, and a narrow read-only import query. They are not process containment.

Use worker children for untrusted, import-heavy, or potentially wedged work. Only child process exit resets Lean
process-global runtime and import state.

## Supported Controls

| Control | Public surface | What it bounds | What it does not provide |
| --- | --- | --- | --- |
| Heartbeats | `LeanElabOptions`, `LeanMetaOptions`, module-query `LeanElabOptions` | Lean operations that already check `Lean.maxHeartbeats` | Hard preemption of arbitrary native or Lean loops |
| Cancellation | `LeanCancellationToken` | Rust-owned boundaries before dispatch and between cancellable bulk items | Interruption of an in-flight Lean call |
| Diagnostic bytes | `LeanElabOptions::diagnostic_byte_limit`, `LeanMetaOptions::diagnostic_byte_limit` | Diagnostic collection returned to Rust | A timeout or memory ceiling for all Lean work |
| Render/output bytes | `ModuleQueryOutputBudgets`, `DeclarationInspectionBudgets` | Rendered module-query, proof-attempt, verification, and inspection fields | Full-file raw info-tree dumps or unbounded pretty output |
| Search rows | `DeclarationSearchRequest::limit` | Declaration-search result rows; Lean also clamps broad limits internally | Exhaustive project indexing |
| Cache clearing | `LeanSession::clear_module_snapshot_cache` | The shim-owned module snapshot cache | Lean runtime reset, module unload, or compacted-region cleanup |
| Import facts | `LeanSession::import_stats`, `LeanImportStats::memory_diagnostic` | Attribution for imports, compacted regions, extensions, and bytes | Reclamation of full-session import state |
| Import admission | `LeanSessionImportProfile`, `SessionPoolMemoryPolicy`, same-process cargo-test import guard | Which full-session imports are admitted and how they are described | Safe repeated cold imports in one long-lived process without a memory policy |
| Bracketed read-only import | `LeanCapabilities::bracketed_import_query` | One-shot declaration metadata under `loadExts := false` with region cleanup | Normal elaboration, parser-backed queries, pretty-printing, or capability workflows |

## Boundary Rules

Full `LeanSession` imports use `loadExts := true`. That is required for parser, elaborator, proof-state,
pretty-printer, source-range, and capability workflows. After extensions are loaded, `Environment.freeRegions` is not a
safe cleanup tool because extension state may retain references into compacted `.olean` regions. Drop and cache clearing
do not change that.

`LeanCapabilities::bracketed_import_query` is the only same-process path that may free compacted import regions. The
Lean shim uses `withImportModules`, which imports with `loadExts := false`, runs a closed declaration-metadata query,
serializes the result to JSON, and calls `Environment.freeRegions` before Rust parses the owned result. No
`Environment`, `Expr`, `Name`, `ConstantInfo`, extension state, session, or capability handle may escape that bracket.

The bracketed path is deliberately not a `LeanSession` profile. If a future read-only query needs to return Lean-owned
objects, defer it until the lifetime across freed compacted regions can be proved locally.

## Non-Guarantees

Same-process controls do not provide:

- hard preemption;
- a full Lean runtime reset;
- recovery after native abort, process exit, foreign unwind, or a wedged Lean runtime;
- safe `Environment.freeRegions` after `loadExts := true`;
- a reason to weaken subprocess isolation for production hosts.

Use `docs/production-hosting.md` for production worker shape and `docs/safety/long-session-memory.md` for the Prompt 29
import-memory baseline. The baseline numbers are local measurements; they explain the retained-memory shape but are not
portable safety constants.

## Testing Shape

Keep tests for these controls narrow:

- unit-test option defaults, saturation, and FFI-boundary normalization without opening full sessions;
- use existing focused host tests for heartbeat timeouts, diagnostic truncation, and pre-cancelled tokens;
- use the bracketed import test to assert `load_exts == false`, `free_regions_ran == true`, and Rust-owned metadata;
- run host integration tests through `cargo nextest`, not broad same-process `cargo test`.
