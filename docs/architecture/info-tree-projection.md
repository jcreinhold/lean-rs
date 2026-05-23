# Info-tree projection

`LeanSession::process_with_info_tree` projects a processed Lean source string into the
[`ProcessedFile`](../../crates/lean-rs-host/src/host/process/info_tree.rs) value type: four arrays of structurally
distinct nodes (commands, terms, tactics, name references) plus the diagnostics the elaborator emitted. One Lean export,
one Rust value, three downstream queries (`goal_at_position`, `type_at_position`, `references_of_name`) unblocked. Per
POSD ch 6.1, the interface stays general-purpose (source, options, cancellation → `ProcessedFile`); the projection's
*functionality* serves the cursor-query trio but its interface doesn't encode any of them.

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

The shim is also explicitly **not** incremental in v1. Every call re-runs `Lean.Elab.IO.processCommands` against the
supplied source — the same path Lean's LSP server uses for each `didChange`. Incremental reuse is a v0.3 optimisation
when there is profile data to justify it. Per-command progress reporting is similarly deferred: every prompt-06 cursor
query operates on one buffer per call, so adding a `_progress` sibling shim would double the symbol contract for a
hypothetical use case.

## Optional capability

`lean_rs_host_process_with_info_tree` is declared **optional** in the
[capability contract](../lean-rs-host-capability-contract.md). A fork of the shim package that omits the symbol still
loads cleanly; `process_with_info_tree` returns `ProcessFileOutcome::Unsupported` at dispatch time without invoking the
FFI. The pattern matches the five `MetaM` services that already use this degradation path
(`LeanMetaResponse::Unsupported`).

## Position helpers

`ProcessedFile` exposes three inherent helpers so cursor consumers do not reinvent the range walk:

- `term_at(line, column) -> Option<&TermInfoNode>` — innermost containing term node.
- `tactic_at(line, column) -> Option<&TacticInfoNode>` — innermost containing tactic node.
- `references_of(name) -> Vec<&NameRefNode>` — every identifier occurrence whose fully-qualified name matches exactly.

"Innermost" is defined by source-range area: lines are weighted heavily so cross-line ranges always dominate single-line
ranges. Ties on the same line break by column span. These helpers are pure Rust — no Lean call.
