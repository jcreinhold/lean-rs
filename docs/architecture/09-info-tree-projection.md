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

## Command Message Capture

Callers that need Lean command output should use the existing diagnostics projection. Build an in-memory module source
from the desired command body, open the worker session with the import context the commands should see, and request
`process_module_query_batch` with one `Diagnostics { id: ... }` selector. Commands such as `#check Nat.add` and
`#print axioms Nat.add_assoc` elaborate through the same `processCommands` path as ordinary module bodies; their
informational output is returned as bounded diagnostics. Invalid commands return error-severity diagnostics, not worker
errors.

No separate `command_message`, `trial`, or `probe` protocol request exists upstream because this is already the generic
module-processing operation. Downstream hosts may wrap the pattern in product-specific names such as `lean_trial`, but
the worker boundary stays the same: one normal module-query request, one terminal outcome, existing cancellation,
timeout, generation, restart, and frame-limit behavior.

This operation is non-mutating in the filesystem sense: `lean-rs` constructs an in-memory module and never writes the
submitted command source to disk. It is not an OS sandbox. Lean commands run inside the worker child process with the
same capabilities as any source the caller asks the worker to elaborate; expensive or effectful commands remain a
downstream policy concern.

## Project-scope scans

A project-scope reference scan is driven by the caller (one per-file query per `.lean` file), not by the worker. The
worker's contribution is to make each per-file query's worst case predictable:

- A file with a **complete** closure pays parse + body elaboration + projection; `elaboration_micros` attributes the
  cost.
- A file with an **incomplete** closure pays only header parsing (the `MissingImports` short-circuit above);
  `elaboration_micros` is 0.

The remaining cross-query cost on a large scan is the *cold header re-import*: when the worker is recycled mid-scan by
the RSS cap (`16-production-boundary.md`, memory cycling and restart policy), the replacement child must re-load the
import closure from `.olean`, which for heavy closures can dominate a per-file query. That is RSS-policy behaviour, not
a projection cost; mitigate it by building the project first or by capping the scan with the caller's `limit`, so the
worker re-imports a closure once and answers many files against it before the next cycle.

## Proof-state locals rendering

`proof_state` locals render through the same pretty, notation-aware delaborator as `goals_before` / `goals_after`, not
through raw `Expr.toString`. The shim threads a single `LocalsRendering` mode (`skip` / `raw` / `pretty`) through every
locals collector so the concern lives in one place:

- **`pretty`** (default for user-facing `proof_state`): each local's type/value is rendered with `Meta.ppExpr` inside
  the goal's `MetaM` context, so binders resolve to user names and notation is applied, then bound under the diagnostic
  byte budget. On a pretty-printer failure it falls back to the raw renderer. This matches the goal renderer and is
  *cheaper* than the raw unfolded form (the raw locals, `((((Quiver.Hom.{v,u} _uniq.NNNN)…`, were the proximate trigger
  of the RSS spike).
- **`raw`**: the previous `Expr`-string form, opt-in via the `locals_raw` selector field.
- **`skip`**: render no locals at all. The verify path and the try-proof-step goals path read only `goals_after` and
  discard locals, so they pass `skip` and pay nothing for locals they never inspect.

`ProofStateInDeclaration` carries one additive wire field, `locals_raw: bool` (`#[serde(default)]`, default `false`):
`false` → `pretty`, `true` → `raw`. The general `proofState` selector is always `pretty` (no flag — the surface stays
tight). The field is backward compatible: an older client that omits it deserializes to `false`, i.e. pretty locals.

## Proof-position selectors and splice placement

A proof position is selected by `ProofPositionSelector`, which resolves to one enumerated tactic state and a *splice
placement* — where a `try_proof_step` candidate is inserted relative to that tactic. The two concerns are kept
orthogonal: the selector picks *which* goal; an internal `TacticPlacement` (`before` / `after`) picks *where* the
candidate lands.

- **`Default` / `Index { index }`**: the first / index-th tactic state — the point *after* that tactic has run.
  `goals_before` is its pre-tactic state, `goals_after` its post-tactic state. A candidate is spliced on a fresh line
  *after* the tactic (placement `after`), so it continues from `goals_after`.
- **`AfterText { text, occurrence }`**: the tactic whose source matches `text`; placement `after`. Falls back to a
  source-text search when no enumerated tactic matches.
- **`Entry`**: the goal state *before any tactic runs* — the pristine entry goal of the proof. It anchors on the first
  enumerated tactic's pre-state (so the declaration must have at least one elaborated tactic) and uses placement
  `before`: the candidate is spliced *before* the first tactic, aligned to that tactic's column at the start of its
  line, so a from-scratch tactic block elaborates against the untouched goal. The original tactics follow below the
  inserted span; once the candidate closes the goal their "no goals" errors fall *outside* the candidate's body span and
  are reported as downstream, not candidate-local. For `proof_state`, `Entry` renders `goals_before == goals_after` (the
  entry goal is unchanged, since nothing has run).

This closes a trap at the `Default` position: `proof_state`'s `goals_before` shows the pristine entry goal, but a
`Default` `try_proof_step` splices *after* the first tactic — so a from-scratch block (e.g. `intro …; exact …`) is run
with the first binder already introduced and fails. `Entry` makes the pristine goal a reachable splice point, not just
an observable one.

## Degraded verdicts under resource pressure

A read-only `verify_declaration` / `proof_state` query elaborated under memory pressure can be *degraded*: the captured
proof state references a metavariable whose decl was evicted, so the elaboration result is untrustworthy even though no
Lean exception was thrown. Three layers keep the verdict honest and the child alive, each at the abstraction where the
signal is authoritative:

1. **In-Lean structural screen (Layer A).** Before any renderer dereferences a captured proof state, a *total* predicate
   walks the goal's target and local-context types for metavariables and checks each against `MetavarContext.findDecl?`
   (the `Option`-returning total query, never the pure-`panic!` `getDecl`). A goal that references a missing mvar
   renders as `<goal unavailable: elaboration degraded under resource pressure>`, and `verificationFacts` skips the
   `unresolvedGoals` and `collectAxioms` walks, leaving `axioms_available = false`. The status router then maps a
   degraded target to `BudgetExceeded`. This prevents the common, directly-reachable abort with no respawn. It is
   best-effort, not a soundness boundary — a dangling mvar reachable only through delayed-assignment machinery can still
   reach the pure `getDecl`.
2. **Worker-child RSS taint (Layer B, silent case).** Memory degradation is frequently silent — no Lean diagnostic, and
   the verdict comes back as a bare `NotFound`/`Rejected`/`Ambiguous` indistinguishable from a genuine name-absent
   answer. After a verify job the child samples its own RSS; if it is at or above `LEAN_RS_VERIFY_RSS_TAINT_KIB` **and**
   the verdict is non-positive (never `Accepted`), it relabels the verdict to `BudgetExceeded` and clears axioms. The
   ceiling is gated high (well above the warm mathlib baseline) and defaults to off (`0`); a missing RSS sample taints
   nothing. The Lean side also reclassifies *loud* cases: a `notFound` whose diagnostics carry a resource marker
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

## Reusing module headers after edits

Each live session keeps at most one reusable header environment, separate from disposable body snapshots. A global
eight-entry least-recently-used bound also limits retention for direct host callers outside the worker pool. Clearing snapshots
because of TTL, size, or RSS no longer forces another header import. Cached queries, uncached queries, proof attempts,
and verification use the same header resolver. Reuse never substitutes the old body's final environment.

Reuse requires the same input environment object, exact header text, file label, heartbeat setting, and complete
Lean option map. Exact text preserves import order, `public import`, `import all`, and source positions. Changing
any of these inputs requires a fresh header. Session eviction clears its header, not the cumulative memory count.
Changing an imported artifact requires a fresh input session or worker restart. Clearing body snapshots alone is not
dependency invalidation: it does not change the session's imported environment.
Host source calls and header-cache operations share the existing process-local session-import mutex. This protects
global Lean import/cache state for direct concurrent host callers; it does not serialize separate worker processes.

The resolver records successful fresh imports before elaborating the body. It sums non-memory-mapped compacted
region sizes, excluding pointer-identical input-session regions and previously charged region objects. It never
deduplicates by filename: loading the same file again can allocate a different buffer. This is conservative retained
import accounting, not a measurement of total heap usage. Metadata copies can overcount; dropping snapshots does
not prove that Lean freed imported regions, so eviction never subtracts bytes.

Before another fresh header, a reached import budget requests a child restart. Warm header reuse remains allowed.
The first header in a fresh child may exceed the budget, under the unchanged runtime memory ceiling; the supervisor
retries at most once. This avoids an endless restart loop for a large but valid first profile. A failed header import
can hide partial allocations. The resolver preserves its error and marks accounting unknown; the supervisor retires
that child before its next request instead of silently retrying the failed import.

Tests compare warm and genuinely cold results after body, visibility, option, file-label, and session changes. A
private Lean probe checks environment identity, snapshot eviction, budget handling, and a failed import. Host API
tests check shared accounting across single and batch queries. These checks do not make C++ exceptions recoverable;
process isolation and the runtime memory ceiling remain necessary.

### Bounded real-project check

`header_cache_probe` runs three diagnostics calls on one existing source file, clearing body snapshots between calls.
It uses one worker thread, a 4 GiB import budget, and a 14 GiB runtime ceiling. It does not build project modules.

```sh
cargo build -p lean-rs-worker-child --bin lean-rs-worker-child --example header_cache_probe
target/debug/examples/header_cache_probe target/debug/lean-rs-worker-child /path/to/project /path/to/source.lean
```

A Mathlib-scale determinant file completed all three calls without errors or restarts: header processing took
3.097 seconds, then 2 milliseconds twice. Recorded retained imports stayed at 3,660,081,344 bytes; RSS stayed near
6.80 GiB and sampled physical footprint peaked at 4.45 GiB. This checks warm reuse after snapshot eviction, not every
profile transition or an upper bound on future elaboration memory.

## File-specific Lean options

The caller supplies Lake's effective per-file option map through `LeanElabOptions::lean_options`
(or `LeanWorkerElabOptions::lean_options`). Boolean, natural-number, and string values retain their types.
Lean decodes the map with its own `LeanOptions` decoder before processing the header or body. Malformed
values fail the request; they never silently select defaults. Natural numbers in this wire format must
fit in `u64`. After imports load, Lean resolves `weak` option prefixes and checks registered names,
just as its normal frontend does. An empty map preserves normal Lean defaults. Source-local `set_option` commands
still work.

The host's heartbeat limit overrides the map's `maxHeartbeats`. This resource override is separate from
semantic options such as `autoImplicit` and `maxSynthPendingDepth`, which must match the build.
All source elaboration entry points, proof overlays, and explicit verification use the map. The worker
cache key, module snapshot lookup, incremental reuse, and header reuse all distinguish option changes.
Worker protocol version 11 prevents an old child from silently ignoring the new map.

Resolving Lake configuration belongs to the caller, not the Lean runtime bridge. The host does not read
or build a project merely to discover options. A caller that omits build options cannot claim that its
result matches that project's build configuration.

The separate `LeanMetaOptions` API still controls standalone MetaM operations such as weak-head
normalization and expression pretty-printing. Those operations do not receive this new map after
their input source has been elaborated. This change establishes build-option parity for source
elaboration and verification, not for every standalone MetaM rendering operation.
