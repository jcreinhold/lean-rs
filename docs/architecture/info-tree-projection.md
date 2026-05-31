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

## Proof-state locals rendering

`proof_state` locals render through the same pretty, notation-aware delaborator as `goals_before` / `goals_after`, not
through raw `Expr.toString`. The shim threads a single `LocalsRendering` mode (`skip` / `raw` / `pretty`) through every
locals collector so the concern lives in one place:

- **`pretty`** (default for user-facing `proof_state`): each local's type/value is rendered with `Meta.ppExpr` inside
  the goal's `MetaM` context, so binders resolve to user names and notation is applied, then bound under the diagnostic
  byte budget. On a pretty-printer failure it falls back to the raw renderer. This matches the goal renderer and is
  *cheaper* than the raw unfolded form (the raw locals, `((((Quiver.Hom.{v,u} _uniq.NNNN)…`, were the proximate trigger
  of the field RSS spike).
- **`raw`**: the previous `Expr`-string form, opt-in via the `locals_raw` selector field.
- **`skip`**: render no locals at all. The verify path and the try-proof-step goals path read only `goals_after` and
  discard locals, so they pass `skip` and pay nothing for locals they never inspect.

`ProofStateInDeclaration` carries one additive wire field, `locals_raw: bool` (`#[serde(default)]`, default `false`):
`false` → `pretty`, `true` → `raw`. The general `proofState` selector is always `pretty` (no flag — the surface stays
tight). The field is backward compatible: an older client that omits it deserializes to `false`, i.e. pretty locals.

## Degraded verdicts under resource pressure

A read-only `verify_declaration` / `proof_state` query elaborated under memory pressure can be *degraded*: the captured
proof state references a metavariable whose decl was evicted, so the elaboration result is untrustworthy even though no
Lean exception was thrown. Three layers keep the verdict honest and the child alive, each at the abstraction where the
signal is authoritative:

1. **In-Lean structural screen (Layer A).** Before any renderer dereferences a captured proof state, a *total* predicate
   walks the goal's target and local-context types for metavariables and checks each against `MetavarContext.findDecl?`
   (the `Option`-returning total query, never the pure-`panic!` `getDecl`). A goal that references a missing mvar renders
   as `<goal unavailable: elaboration degraded under resource pressure>`, and `verificationFacts` skips the
   `unresolvedGoals` and `collectAxioms` walks, leaving `axioms_available = false`. The status router then maps a
   degraded target to `BudgetExceeded`. This prevents the common, directly-reachable abort with no respawn. It is
   best-effort, not a soundness boundary — a dangling mvar reachable only through delayed-assignment machinery can still
   reach the pure `getDecl`.
2. **Worker-child RSS taint (Layer B, silent case).** Memory degradation is frequently silent — no Lean diagnostic, and
   the verdict comes back as a bare `NotFound`/`Rejected`/`Ambiguous` indistinguishable from a genuine name-absent
   answer. After a verify job the child samples its own RSS; if it is at or above `LEAN_RS_VERIFY_RSS_TAINT_KIB` **and**
   the verdict is non-positive (never `Accepted`), it relabels the verdict to `BudgetExceeded` and clears axioms. The
   ceiling is gated high (well above the warm mathlib baseline) and defaults to off (`0`); a missing RSS sample taints
   nothing. The Lean side additionally reclassifies *loud* cases: a `notFound` whose diagnostics carry a resource marker
   (`diagnosticIndicatesResourceLimit`: deep recursion, interrupted, out of memory) becomes `timeout` or
   `BudgetExceeded` rather than `NotFound`.
3. **Supervisor verdict-on-abort (Layer C, authoritative backstop).** A residual abort that escapes Layer A is an
   uncatchable process exit, so only the parent can absorb it. `worker_verify_declaration` and
   `worker_process_module_query_batch` map a `ChildPanicOrAbort` during the request to a synthesized degraded result —
   verify → `Ok { BudgetExceeded, facts: unavailable(), imports: [] }`; batch → a per-selector `BudgetExceeded` outcome
   — and record a `ChildAbort` restart (`stable_cause = "child_abort"`). The caller always gets a verdict; the next call
   is served by a freshly respawned child. This is the process-boundary half of the panic-containment contract; see
   [`06-panic-containment.md`](06-panic-containment.md).

No new status variant is introduced: `BudgetExceeded` already names "the worker could not complete this within its
resource envelope," which is exactly the degraded case. `LEAN_RS_VERIFY_RSS_TAINT_KIB` is an internal env var plumbed
from the pool's per-worker RSS ceiling by `LeanWorkerModuleCacheLimits::verify_rss_taint_kib`, mirroring
`LEAN_RS_MODULE_CACHE_RSS_GUARD_KIB`; it is not a public knob.
