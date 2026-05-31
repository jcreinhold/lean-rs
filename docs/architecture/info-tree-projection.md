# Module-query projection

`LeanSession::process_module_query` is the public module-processing boundary. Callers submit a `ModuleQuery`; the Lean
shim parses the file header, elaborates the body for that request, performs any info-tree traversal in Lean, and returns
only the requested bounded projection.

The old whole-file `ProcessedFile` dump is intentionally gone. Diagnostics, cursor type lookup, cursor goal lookup, and
name-reference lookup no longer serialize every command, term, tactic, raw expression string, inferred type string, and
expected type string in the file.

## Query Shape

| Query | Result |
| --- | --- |
| `Diagnostics` | `LeanElabFailure` only. No term, tactic, or name-reference projection is walked or serialized. |
| `TypeAt { line, column }` | The innermost `TermInfo` covering the 1-indexed cursor, with bounded expression/type/expected-type rendering. |
| `GoalAt { line, column }` | The innermost `TacticInfo` covering the cursor, with before/after goals rendered under the diagnostic byte budget. |
| `References { name }` | Binder/use-site locations whose recorded name matches exactly. Expression and type text is never rendered. |

All traversal and rendering policy stays Lean-owned. Rust chooses the query and decodes the result; it does not
reconstruct info-tree semantics from a broad internal dump.

## Outcome Shape

`ModuleQueryOutcome` distinguishes transport capability from module-header state:

- `Ok { result, imports }`—header parsed, all user-written imports are present in the session environment, and the query
  result is available.
- `MissingImports { result, imports, missing }`—header parsed, but some user imports are absent from the session
  environment's transitive module closure. The body is **not** elaborated in this case: with the imports absent the
  environment lacks the file's symbols, so a full elaboration would only produce unknown-symbol errors whose projection
  the caller discards as a `needs_build` / `files_skipped` degrade. The shim short-circuits to an empty projection
  (empty references, empty diagnostics, no candidates) with `elaboration_micros = 0`, and the caller reads `missing` to
  degrade. This bounds the cost of a query against an incomplete file to header parsing.
- `HeaderParseFailed { diagnostics }`—header parsing failed; body elaboration and info-tree traversal do not run.
- `Unsupported`—the loaded capability dylib does not export `lean_rs_host_process_module_query`.

Module-system headers keep the same import reporting policy as ordinary headers: `module`, `public import`, ordinary
`import`, and `import all` report bare module names, without modifiers.

## Bounds

Budgets are internal policy, not public knobs:

- diagnostics and tactic goals use `LeanElabOptions::diagnostic_byte_limit`;
- type rendering uses a private bounded expression renderer;
- references stop at an internal cap and set `truncated`.

This keeps the API narrow and makes frame size a consequence of query shape. Worker frame limits remain a final
transport guard, not the first place oversized full-file expression strings are discovered.

## Project-scope scans

A project-scope reference scan is driven by the caller (one per-file query per `.lean` file), not by the worker. The
worker's contribution is to make each per-file query's worst case predictable:

- A file with a **complete** closure pays parse + body elaboration + projection; `elaboration_micros` attributes the
  cost.
- A file with an **incomplete** closure pays only header parsing (the `MissingImports` short-circuit above);
  `elaboration_micros` is 0.

The remaining cross-query cost on a large scan is the *cold header re-import*: when the worker is recycled mid-scan by
the RSS cap (`16-production-boundary.md`, memory cycling and restart policy), the replacement child must re-load the import
closure from `.olean`, which for heavy closures can dominate a per-file query. That is RSS-policy behaviour, not a
projection cost; mitigate it by building the project first or by capping the scan with the caller's `limit`, so the
worker re-imports a closure once and answers many files against it before the next cycle.
