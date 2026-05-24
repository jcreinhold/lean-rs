# Info-tree projection

Two sibling session methods project a processed Lean source into the
[`ProcessedFile`](../../crates/lean-rs-host/src/host/process/info_tree.rs) value type: four arrays of structurally
distinct nodes (commands, terms, tactics, name references) plus the diagnostics the elaborator emitted. The two methods
answer different questions and live behind different Lean shim exports, but share the projection walker verbatim:

| Method | Lean shim | When to use |
| --- | --- | --- |
| `LeanSession::process_with_info_tree` | `lean_rs_host_process_with_info_tree` | Body-only snippet, no header. The shim runs `IO.processCommands` from byte 0 with an empty `ModuleParserState`. Right for inline scratch buffers and tactic-level snippets. |
| `LeanSession::process_module_with_info_tree` | `lean_rs_host_process_module_with_info_tree` | Full Lean source file (header + body). The shim calls `Lean.Parser.parseHeader` first and resumes `IO.processCommands` from the parser state the header parser produced. Positions in the returned projection land in the original file's line/column system. Right for real-file inputs and downstream position tools. |

Both methods feed the same downstream cursor queries (`goal_at_position`, `type_at_position`, `references_of_name`,
`term_at`), which consume `ProcessedFile` and don't care which method produced it. The split is by question, not by
flag: a `mode` parameter on a single shim would push the snippet-vs-file choice into every caller. Both interfaces stay
general-purpose (source, options, cancellation → outcome) so the projection serves the cursor-query set without
encoding any of those queries in its signature.

## What the projection carries

Every node carries an explicit `(start_line, start_column, end_line, end_column)` source range — 1-based at every layer,
matching Lean's `Position` convention. Bodies are owned strings and primitive integers only, so a `ProcessedFile` is
`Send + Sync + 'static` and crosses worker-thread channels cleanly.

| Node | What it is |
| --- | --- |
| `CommandInfoNode` | One top-level command. `decl_name` is set for declaration commands (`def`, `theorem`, `instance`, …) and `None` for others (`#check`, `open`, comment-only fragments). |
| `TermInfoNode` | One `Lean.Elab.TermInfo` node — an elaborated expression with raw `Expr.toString` text plus the inferred type. `expected_type_str` is set where the elaborator recorded a coercion target. |
| `TacticInfoNode` | One `Lean.Elab.TacticInfo` node — the tactic's source range plus already-pretty-printed `goals_before` / `goals_after`. Goal strings come from `Lean.Meta.ppGoal` inside the elaboration's MetaM context, so no metavariable identity has to cross the FFI. |
| `NameRefNode` | One identifier occurrence. `is_binder` distinguishes binding sites from use sites — the same distinction Lean's LSP uses to answer "go to definition" vs. "find references". |

The diagnostics field reuses the host stack's `LeanElabFailure` shape, so callers branch through the same
`diagnostics()` / `truncated()` accessors as `LeanSession::kernel_check`.

## What the projection does *not* carry

Raw `Lean.Expr` values, metavariable contexts, and `Elab.InfoTree` nodes themselves all stay behind the FFI boundary on
purpose. They carry references the Rust side cannot revive outside the elaboration session that produced them —
projecting to strings + ranges is what keeps the cross-thread guarantee. Callers that need notation-aware text for a
specific expression use the optional `lean_rs_host::meta::pp_expr` service against the captured expression on the Lean
side, not the projection.

The shim is not incremental. Every call re-runs `Lean.Elab.IO.processCommands` against the supplied source — the same
path Lean's LSP server uses for each `didChange`. Incremental reuse is a future optimisation, gated on profile data.
Per-command progress reporting is similarly out of scope: each cursor query operates on one buffer per call, so a
`_progress` sibling shim would double the symbol contract without a concrete use case.

## Outcome shape

`process_with_info_tree` returns a two-arm `ProcessFileOutcome` (`Processed` + `Unsupported`). The header-aware
`process_module_with_info_tree` returns a four-arm `ProcessModuleOutcome`:

- `Ok { file, imports }` — header parsed; every user-written import is present in the session's open env's transitive
  module closure; the body was processed.
- `MissingImports { file, imports, missing }` — header parsed but some imports name modules absent from the env's
  transitive closure. The body still elaborated; the projection is populated. Soft failure — callers typically surface
  it as a warning.
- `HeaderParseFailed { diagnostics }` — `Lean.Parser.parseHeader` reported error-severity messages. `IO.processCommands`
  was not run.
- `Unsupported` — the loaded capability dylib does not export the new symbol. No FFI call was made.

The "missing imports" check compares against `env.header.moduleNames` (the transitive closure), not `env.header.imports`
(only direct imports). Otherwise a session that imports `LeanRsHostShims.Elaboration` — which transitively pulls in
`Lean` — would flag every `import Lean` in user files as missing.

## Optional capability

Both shim symbols are declared **optional** in the [capability contract](../lean-rs-host-capability-contract.md). A fork
of the shim package that omits either symbol still loads cleanly; the corresponding session method returns its
`Unsupported` arm at dispatch time without invoking the FFI. The pattern matches the five `MetaM` services that already
use this degradation path (`LeanMetaResponse::Unsupported`).

## Position helpers

`ProcessedFile` exposes three inherent helpers so cursor consumers do not reinvent the range walk:

- `term_at(line, column) -> Option<&TermInfoNode>` — innermost containing term node.
- `tactic_at(line, column) -> Option<&TacticInfoNode>` — innermost containing tactic node.
- `references_of(name) -> Vec<&NameRefNode>` — every identifier occurrence whose fully-qualified name matches exactly.

"Innermost" is defined by source-range area: lines are weighted heavily so cross-line ranges always dominate single-line
ranges. Ties on the same line break by column span. These helpers are pure Rust — no Lean call.
