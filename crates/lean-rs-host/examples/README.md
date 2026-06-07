# lean-rs examples

**New to this project? Start here.** The examples below drive the curated `lean_rs::*` and `lean_rs_host::*` surfaces
end to end against the in-tree fixture; reading them is the fastest path from a blank consumer project to a working
integration.

Five focused examples, one end-to-end tour, and one RSS reproducer. Each focused example sticks to one verb so a reader
can scan it in a minute and reach for the matching crate API by analogy.

| Example | What it teaches |
| --- | --- |
| [`theorem_query`](#theorem_query) | Open a session and contrast the kind of a Lean definition with a theorem. |
| [`proof_check`](#proof_check) | Kernel-check a theorem, re-validate the evidence, project a bounded summary. |
| [`meta_query`](#meta_query) | Run a bounded `MetaM` service and branch on every status. |
| [`progress`](#progress) | Observe structured progress and cooperative cancellation on long-running calls. |
| [`tour`](#tour) | Core host flows composed end to end in one process. |
| [`lake_build_helper`](#lake_build_helper) | Build a Lake shared-library target through `lean-toolchain` without hand-written dylib path mangling. |
| [`long_session_memory`](#long_session_memory) | RSS checkpoints for a long-lived process using fresh imports, pooled reuse, introspection, and elaboration. |

## Prerequisites

Every example loads the in-tree fixture Lake project from `fixtures/lean/`. Build it once before running any example:

```sh
cd fixtures/lean && lake build && cd -
```

The examples panic at `LeanRuntime::init` time with a clear diagnostic if the build is missing.

## Tracing output

Each example installs a `tracing_subscriber::fmt` sink that respects `RUST_LOG`. The defaults stay quiet so the
example's own `println` lines aren't drowned out; raise the level to see structured spans:

```sh
RUST_LOG=lean_rs=info  cargo run -p lean-rs --example theorem_query
RUST_LOG=lean_rs=debug cargo run -p lean-rs --example proof_check
RUST_LOG=lean_rs=trace cargo run -p lean-rs --example meta_query
```

See [`docs/diagnostics.md`](../../../docs/diagnostics.md) for the full span catalogue and the recommended `RUST_LOG`
scope per workload.

## Error output

Every example wraps its happy-path work in `fn run() -> LeanResult<()>` and propagates with `?`. `main` prints the
failure with the diagnostic code in brackets and exits non-zero:

```text
[lean_rs.linking] lean-rs: host stage Link [lean_rs.linking]: ...
```

The code names the failure family (see the [`LeanDiagnosticCode`](../../../docs/diagnostics.md#diagnostic-codes)
catalogue); the bracketed text is `as_str()` and is stable across patch releases.

## Examples

### theorem_query

**Goal:** open a session over the fixture Lake project and contrast how Lean classifies a definition (`Nat.add`) versus
a theorem (`Nat.add_zero`).

**Run:**

```sh
cargo run -p lean-rs --example theorem_query
```

**Expected output:**

```text
total_declarations=189017
Nat.add: kind=definition rendered=Nat.add
Nat.add_zero: kind=theorem rendered=Nat.add_zero
ok
```

The exact `total_declarations` count tracks the active Lean toolchain prelude and will drift with the version pinned in
`lean-toolchain`.

**Common failures:**

- `[lean_rs.module_init] ... 'fixtures/lean' does not exist or is not a directory`—run from a directory other than the
  workspace root. Either `cd` to the workspace root or set `CARGO_MANIFEST_DIR` appropriately.
- `[lean_rs.module_init] ... failed to open Lean library`—the Lake fixture has not been built. See *Prerequisites*.
- `[lean_rs.abi_conversion] declaration 'Nat.add_zero' not found in imported environment`—the imported module set
  excludes the prelude; only happens if the example's import list is edited.

### proof_check

**Goal:** submit a small theorem to `LeanSession::kernel_check`, re-validate the resulting evidence with
`check_evidence`, and print the bounded `ProofSummary`.

**Run:**

```sh
cargo run -p lean-rs --example proof_check
```

**Expected output:**

```text
kernel_check source: theorem demo_proof_check : 1 + 1 = 2 := rfl
check_evidence: Checked
summary: name=demo_proof_check kind=theorem type=Eq.{1} Nat ...
```

The `type=` rendering is Lean's pretty printer and tracks the active toolchain—the prefix `Eq.{1} Nat ...` is stable for
this theorem but the exact bytes will drift with Lean version.

**Common failures:**

- The kernel-check outcome prints `kernel rejected the proof: ...`—the proof term is bad. Edit the source string in
  `examples/proof_check.rs` and re-run.
- `[lean_rs.lean_exception] Lean threw ...`—the elaboration shim raised through IO before the kernel saw the term. The
  bounded message names the cause.

### meta_query

**Goal:** elaborate `(Nat.succ 0 : Nat)` to a `LeanExpr`, run the `infer_type` `MetaM` service against it, and branch on
every `LeanMetaResponse` status.

**Run:**

```sh
cargo run -p lean-rs --example meta_query
```

**Expected output:**

```text
status=Ok service=infer_type
ok
```

**Common failures:**

- `status=Unsupported: ...`—the capability dylib lacks the `lean_rs_host_meta_infer_type` shim. Rebuild the fixture; the
  in-tree fixture exports all four meta services (`infer_type`, `whnf`, `heartbeat_burn`, `is_def_eq`).
- `status=TimeoutOrHeartbeat: ...`—the heartbeat ceiling tripped before `infer_type` finished. Raise
  `LeanMetaOptions::new() .heartbeat_limit(N)`.

### progress

**Goal:** attach a `LeanProgressSink` to a bulk query, then show a progress sink triggering a shared
`LeanCancellationToken`.

**Run:**

```sh
cargo run -p lean-rs-host --example progress
```

**Expected output:** one or more `progress phase=... current=... total=...` lines, followed by `queried_declarations=3`,
a `cancel_progress ...` line, `cancelled_code=lean_rs.cancelled`, and `ok`.

**Common failures:**

- `[lean_rs.module_init] ... failed to open Lean library`—the Lake fixture has not been built. See *Prerequisites*.
- No progress output—check that the example passes `Some(&sink)` as the final progress argument; `None` is intentionally
  silent and keeps the fast path.

### tour

**Goal:** see how the four focused examples compose into one end-to-end workflow: host open → capability load → session
import → two `elaborate` calls → one `kernel_check` → one bulk declaration query → one `Meta.whnf`. Output is per-stage
wall-clock for performance triage (see [`docs/performance.md`](../../../docs/performance.md)).

**Run:**

```sh
cargo run -p lean-rs --example tour
```

**Expected output:** one `name=<stage> elapsed_us=<u64>` line per stage, suitable for `grep`/`awk`. The exact
`elapsed_us` values are machine-dependent.

### lake_build_helper

**Goal:** demonstrate the downstream `build.rs` helper that runs `lake build <target>:shared`, caches the result, and
returns the produced dylib path.

**Run:**

```sh
cargo run -p lean-rs-host --example lake_build_helper
```

**Expected output:** `cargo:rerun-if-changed=...` lines from the helper, followed by:

```text
dylib=.../.lake/build/lib/liblean__rs__fixture_LeanRsFixture.dylib
```

The extension is `.so` on Linux. This example intentionally prints Cargo directives because the helper is meant for
build scripts.

### long_session_memory

**Goal:** characterize retained RSS across long-session lifetime boundaries: runtime initialization, capability loading,
repeated fresh imports, bounded `SessionPool` reuse, bulk declaration queries, elaboration, and session/pool drops.

**Run:**

```sh
LEAN_RS_NUM_THREADS=1 cargo run --release -p lean-rs-host --example long_session_memory
```

**Expected output:** stable `key=value` lines including `lean_version`, workload parameters, `pool_stats=...`, and
`checkpoint=<stage> rss_kib=<u64>`.

Defaults are bounded. Raise `LEAN_RS_LONG_SESSION_IMPORTS` only after the previous run's peak RSS is acceptable, and set
`LEAN_RS_LONG_SESSION_MAX_RSS_KIB` to make the example refuse the next fresh import before crossing a local ceiling.
The same workload is wrapped by `profiling/scripts/profile_memory.sh long-session` and
`profiling/scripts/profile_with_samply.sh long-session`.

This example is intentionally not a Criterion bench. It answers a retained-memory question over minutes and lifetime
boundaries; Criterion answers per-iteration latency questions. The measured model and consumer guidance live in
[`docs/safety/long-session-memory.md`](../../../docs/safety/long-session-memory.md).

## Same-process callback interop

Callbacks are a `lean-rs` feature, not a `lean-rs-host` session feature. Run the generic interop example from the
workspace root:

```sh
cargo run -p lean-rs --example interop_callback
```

That example builds the bundled `lean-rs-interop-shims` package and a downstream-style Lake target with
`lean_toolchain::build_lake_target`, then invokes a Lean export that calls back into a Rust `LeanCallbackHandle`.

## Pointers

- Diagnostic catalogue: [`docs/diagnostics.md`](../../../docs/diagnostics.md)
- Concurrency contract: [`docs/architecture/04-concurrency.md`](../../../docs/architecture/04-concurrency.md)
- Curated public surface: [`docs/architecture/03-host-stack.md`](../../../docs/architecture/03-host-stack.md)
- Performance: [`docs/performance.md`](../../../docs/performance.md)
