# Profiling and Diagnostics

How to find the bottleneck in slow Lean 4 code. Always start here before applying fixes.

## Diagnostics Mode (Start Here)

The single most useful option for performance triage:

```lean
set_option diagnostics true in
set_option diagnostics.threshold 10 in  -- report counters above this value
theorem slow_theorem : ... := by ...
```

This tracks counters across subsystems and reports:

- **Typeclass synthesis**: how many times each instance was tried, how many unfoldings per declaration, which instances
  dominate.
- **Simp**: how many times each lemma was tried vs. actually used. A lemma tried 500 times but used 0 is a candidate for
  removal from the simp set.
- **Kernel**: unfolds per definition. High counts indicate expensive reduction.

Read the output top-to-bottom. The subsystem consuming the most heartbeats is the bottleneck.

## Heartbeat Counting

A heartbeat equals roughly 1,000 small memory allocations. The default budget is 200,000 heartbeats per command.

```lean
-- Measure actual heartbeat usage (mathlib utility)
count_heartbeats in
theorem my_theorem : ... := by ...

-- Raise the limit for a specific command (only after optimizing)
set_option maxHeartbeats 400000 in
theorem genuinely_complex : ... := by ...
```

Use `count_heartbeats in` as a regression watermark: if an unrelated change makes a lemma slower, the heartbeat count
catches it.

Related budgets:

- `synthInstance.maxHeartbeats` (default 20,000) — per typeclass resolution
- `maxRecDepth` — maximum recursion depth for Lean procedures

## Wall-Clock Profiler

Shows time spent in elaboration, typeclass inference, and type checking:

```lean
set_option profiler true in
set_option profiler.threshold 10 in  -- ms threshold (default 100)
theorem my_theorem : ... := by ...
```

This gives a coarse breakdown by phase. Use it to distinguish "elaboration is slow" from "kernel checking is slow."

## Tactic-Level Trace Profiler

For per-tactic granularity:

```lean
set_option trace.profiler true in
set_option trace.profiler.threshold 10 in  -- ms threshold
theorem my_theorem : ... := by ...
```

This produces a hierarchical trace in the Infoview showing time per tactic step. Look for the single tactic consuming
the most time — that is your target.

## Firefox Profiler Integration

For flame-graph visualization:

```lean
set_option trace.profiler.output "/tmp/profile.json" in
set_option trace.profiler.output.pp true in
theorem my_theorem : ... := by ...
```

Then open `/tmp/profile.json` at `profiler.firefox.com`. The flame graph shows the full call stack, making it easy to
spot where time concentrates.

Command-line equivalent:

```bash
lake env lean \
  -Dtrace.profiler.output=profile.json \
  -Dtrace.profiler.output.pp=true \
  MyModule.lean
```

## Targeted Trace Options

When you know the subsystem, use targeted traces:

```lean
-- Typeclass synthesis: see which instances are tried and in what order
set_option trace.Meta.synthInstance true in

-- Simp: see which rewrite rules fire
set_option trace.Meta.Tactic.simp.rewrite true in

-- Unification: see definitional equality checks
set_option trace.Meta.isDefEq true in
```

These produce verbose output. Use them on a single slow command, not a whole file.

## Simp Loop Detection

If `simp` hangs or times out, it may be looping:

```lean
-- After a simp timeout, Lean automatically runs this check.
-- You can also enable it manually:
set_option linter.loopingSimpArgs true in
```

This simplifies the RHS of each candidate lemma to detect cycles. It is expensive, so only enable it when diagnosing a
suspected loop.

## Profiling Workflow

1. Add `set_option diagnostics true in` before the slow command.
1. Read the heartbeat breakdown. Identify the dominant subsystem.
1. If elaboration dominates:
    - Add `set_option profiler true in` for phase breakdown.
    - Add `set_option trace.profiler true in` for per-tactic detail.
1. If typeclass synthesis dominates:
    - Add `set_option trace.Meta.synthInstance true in`.
    - Look for repeated failed attempts at the same goal.
1. If simp dominates:
    - Add `set_option trace.Meta.Tactic.simp.rewrite true in`.
    - Look for lemmas tried many times but never used.
1. After fixing, measure again with `count_heartbeats in` to confirm improvement.
