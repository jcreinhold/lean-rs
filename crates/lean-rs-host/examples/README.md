# lean-rs examples

Four focused single-concern examples plus one end-to-end tour, all
driving the curated `lean_rs::*` surface. Each focused example sticks
to one verb so a reader can scan it in a minute and reach for the
matching crate API by analogy.

| Example | What it teaches |
| --- | --- |
| [`theorem_query`](#theorem_query) | Open a session and contrast the kind of a Lean definition with a theorem. |
| [`evaluate`](#evaluate) | Call a typed Lean export through `LeanSession::call_capability`. |
| [`proof_check`](#proof_check) | Kernel-check a theorem, re-validate the evidence, project a bounded summary. |
| [`meta_query`](#meta_query) | Run a bounded `MetaM` service and branch on every status. |
| [`tour`](#tour) | All four flows composed end to end in one process. |

## Prerequisites

Every example loads the in-tree fixture Lake project from
`fixtures/lean/`. Build it once before running any example:

```sh
cd fixtures/lean && lake build && cd -
```

The examples panic at `LeanRuntime::init` time with a clear diagnostic
if the build is missing.

## Tracing output

Each example installs a `tracing_subscriber::fmt` sink that respects
`RUST_LOG`. The defaults stay quiet so the example's own `println`
lines aren't drowned out; raise the level to see structured spans:

```sh
RUST_LOG=lean_rs=info  cargo run -p lean-rs --example theorem_query
RUST_LOG=lean_rs=debug cargo run -p lean-rs --example proof_check
RUST_LOG=lean_rs=trace cargo run -p lean-rs --example meta_query
```

See [`docs/diagnostics.md`](../../../docs/diagnostics.md) for the
full span catalogue and the recommended `RUST_LOG` scope per
workload.

## Error output

Every example wraps its happy-path work in `fn run() -> LeanResult<()>`
and propagates with `?`. `main` prints the failure with the diagnostic
code in brackets and exits non-zero:

```text
[lean_rs.linking] lean-rs: host stage Link [lean_rs.linking]: ...
```

The code names the failure family (see the
[`LeanDiagnosticCode`](../../../docs/diagnostics.md#diagnostic-codes)
catalogue); the bracketed text is `as_str()` and is stable across
patch releases.

## Examples

### theorem_query

**Goal:** open a session over the fixture Lake project and contrast
how Lean classifies a definition (`Nat.add`) versus a theorem
(`Nat.add_zero`).

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

The exact `total_declarations` count tracks the active Lean
toolchain prelude and will drift with the version pinned in
`lean-toolchain`.

**Common failures:**

- `[lean_rs.module_init] ... 'fixtures/lean' does not exist or is not
  a directory` — run from a directory other than the workspace root.
  Either `cd` to the workspace root or set `CARGO_MANIFEST_DIR`
  appropriately.
- `[lean_rs.module_init] ... failed to open Lean library` — the Lake
  fixture has not been built. See *Prerequisites*.
- `[lean_rs.abi_conversion] declaration 'Nat.add_zero' not found in
  imported environment` — the imported module set excludes the
  prelude; only happens if the example's import list is edited.

### evaluate

**Goal:** call a typed `@[export]` Lean function from Rust through
`LeanSession::call_capability` with both a boxed (`String`) and an
unboxed (`u32`) signature.

**Run:**

```sh
cargo run -p lean-rs --example evaluate
```

**Expected output:**

```text
string_identity("hello, lean") = "hello, lean"
u32_add(1000, 2500) = 3500
ok
```

**Common failures:**

- `[lean_rs.symbol_lookup] unknown exported symbol
  'lean_rs_fixture_string_identity' in ...` — the capability dylib
  was built without the `LeanRsFixture.Strings` module's
  `@[export]`. Re-run `lake build`.
- `[lean_rs.abi_conversion] ...` — the example's hardcoded argument
  type drifted from the Lean signature. Re-check the Rust
  `(args), R` triple against `fixtures/lean/LeanRsFixture/Strings.lean`
  and `Scalars.lean`.

### proof_check

**Goal:** submit a small theorem to `LeanSession::kernel_check`,
re-validate the resulting evidence with `check_evidence`, and print
the bounded `ProofSummary`.

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

The `type=` rendering is Lean's pretty printer and tracks the active
toolchain — the prefix `Eq.{1} Nat ...` is stable for this theorem
but the exact bytes will drift with Lean version.

**Common failures:**

- The kernel-check outcome prints `kernel rejected the proof: ...` —
  the proof term is bad. Edit the source string in
  `examples/proof_check.rs` and re-run.
- `[lean_rs.lean_exception] Lean threw ...` — the elaboration shim
  raised through IO before the kernel saw the term. The bounded
  message names the cause.

### meta_query

**Goal:** elaborate `(Nat.succ 0 : Nat)` to a `LeanExpr`, run the
`infer_type` `MetaM` service against it, and branch on every
`LeanMetaResponse` status.

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

- `status=Unsupported: ...` — the capability dylib lacks the
  `lean_rs_host_meta_infer_type` shim. Rebuild the fixture; the
  in-tree fixture exports all three meta services
  (`infer_type`, `whnf`, `heartbeat_burn`).
- `status=TimeoutOrHeartbeat: ...` — the heartbeat ceiling tripped
  before `infer_type` finished. Raise `LeanMetaOptions::new()
  .heartbeat_limit(N)`.

### tour

**Goal:** see how the four focused examples compose into one
end-to-end workflow: host open → capability load → session import →
two `elaborate` calls → one `kernel_check` → one bulk declaration
query → one `Meta.whnf`. Output is per-stage wall-clock for
performance triage (see
[`docs/performance/interventions.md`](../../../docs/performance/interventions.md)).

**Run:**

```sh
cargo run -p lean-rs --example tour
```

**Expected output:** one `name=<stage> elapsed_us=<u64>` line per
stage, suitable for `grep`/`awk`. The exact `elapsed_us` values are
machine-dependent.

## Why no `callbacks` example?

Rust-side callbacks invoked from Lean are intentionally not on the
public surface today. The infrastructure for the panic-containment
boundary exists (`LeanError::callback_panic`, the `CallbackPanic`
host stage), but no registration path has landed: no fixture
`@[export]` accepts a Rust function handle. Shipping a `callbacks`
example would mean adding speculative public surface, which the
project explicitly avoids.

When a real downstream caller needs Rust callbacks, the
panic-containment seam in `crates/lean-rs/src/error/panic.rs` is the
attach point, and a focused example will follow the same shape as the
four above.

## Pointers

- Diagnostic catalogue: [`docs/diagnostics.md`](../../../docs/diagnostics.md)
- Concurrency contract: [`docs/architecture/04-concurrency.md`](../../../docs/architecture/04-concurrency.md)
- Curated public surface: [`docs/architecture/04-host-stack.md`](../../../docs/architecture/04-host-stack.md)
- Performance baseline: [`docs/performance/baseline.md`](../../../docs/performance/baseline.md)
