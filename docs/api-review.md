# lean-rs public-API audit (prompt 27)

> **Superseded for shape by RD-2026-05-18-001 (2026-05-18).** This document is
> the historical record of the prompt-27 pre-publish audit, captured when the
> workspace had three published crates. RD-2026-05-18-001 split
> `lean-rs::host` into the sibling crate `lean-rs-host`; the publish set is
> now four crates (`lean-rs-sys`, `lean-toolchain`, `lean-rs`, `lean-rs-host`)
> and the classification table for the L2 host stack lives at
> `docs/architecture/04-host-stack.md`. The five-category framework and the
> demotions / pass-through / smell verdicts below remain accurate per-item;
> only the per-crate placement narrative shifted (items previously under
> `lean_rs::host::*` now live at `lean_rs_host::*` and
> `lean_rs_host::host::*`). Inline call-outs flag the most-misleading
> per-crate statements; see the live baselines in `docs/api-review/` and
> the four-crate dry-run table in `docs/release.md` for current state.

This document records the pre-release public-API audit for the published
crates carried out under prompt 27. The audit predates the v0.1 publish/tag
work in prompt 28 and the RD-2026-05-18-001 L1/L2 split.

## Scope

The audit covered every public item in:

- `lean-rs-sys` 0.1.0 — raw Lean 4 C ABI bindings.
- `lean-toolchain` 0.1.0 — toolchain discovery, fingerprint, link diagnostics,
  build-script helpers.
- `lean-rs` 0.1.0 — at audit time, the safe front door with `error`, `module`,
  `host` public modules. Per RD-2026-05-18-001, `host` (and its sub-modules)
  moved to the sibling crate `lean-rs-host` 0.1.0; `lean-rs` now exposes
  `error`, `module`, `handle`, `runtime`, `abi` as the L1 typed-FFI primitive.

Out of scope: extern declarations and the `REQUIRED_SYMBOLS` allowlist of
`lean-rs-sys` (changes there go through a separate replanning delta);
release tagging (prompt 28); new capabilities; performance changes not driven
by the audit.

## Baseline

`cargo public-api --simplified` baselines are committed under
[`docs/api-review/`](api-review/):

- [`lean-rs-sys-public.txt`](api-review/lean-rs-sys-public.txt) — 259 lines.
- [`lean-toolchain-public.txt`](api-review/lean-toolchain-public.txt) — 124 lines.
- [`lean-rs-public.txt`](api-review/lean-rs-public.txt) — 1159 lines (after
  the RD-2026-05-18-001 split, down from 1441 lines pre-split).
- [`lean-rs-host-public.txt`](api-review/lean-rs-host-public.txt) — 899 lines
  (added by RD-2026-05-18-001).

Later prompts and PRs that touch the public surface diff against these files;
intentional surface changes regenerate the baseline as part of the same
commit. The `--simplified` flag strips blanket auto-trait impls so the diff
stays signal-heavy.

Regenerate with:

```sh
cargo public-api -p lean-rs-sys    --simplified > docs/api-review/lean-rs-sys-public.txt
cargo public-api -p lean-toolchain --simplified > docs/api-review/lean-toolchain-public.txt
cargo public-api -p lean-rs        --simplified > docs/api-review/lean-rs-public.txt
cargo public-api -p lean-rs-host   --simplified > docs/api-review/lean-rs-host-public.txt
```

## Classification

The five categories the audit places every public item into:

1. **Raw FFI entry point** (`lean-rs-sys` only) — opaque `lean_object` plus
   `pub unsafe fn` helpers; every public item carries a `# Safety` section
   per `RD-2026-05-17-005`.
2. **Toolchain metadata / build helper** (`lean-toolchain` only).
3. **Curated entry point** at the `lean-rs` crate root (`lean_rs::*`) — every
   item promoted to the crate root by the prompt-18 `pub use` blocks.
4. **Module-level escape hatch** under `lean_rs::module`, `::host`, or
   `::error` — visible to power users but not promoted to the crate root.
5. **Internal-but-accidentally-public** — items that should be `pub(crate)`.

Per-crate placement:

- `lean-rs-sys`: every public item is in category 1. Module structure
  (`array`, `closure`, `consts`, `ctor`, `external`, `io`, `init`,
  `nat_int`, `object`, `refcount`, `scalar`, `string`, `types`) splits by
  Lean header category.
- `lean-toolchain`: every public item is in category 2. The re-exports
  `LEAN_HEADER_DIGEST` / `LEAN_HEADER_PATH` / `LEAN_VERSION` are pass-through
  from `lean-rs-sys`'s `consts` module so a downstream `build.rs` can read
  them without depending on `lean-rs-sys` directly.
- `lean-rs`: classification matches `docs/architecture/04-host-stack.md`
  line-for-line (rather than restating it here):
  - **Crate root (category 3)** — entry points and mandatory session
    capabilities. The six `pub use` blocks in
    `crates/lean-rs/src/lib.rs:105–118` are the authoritative list.
  - **Module-path escape hatches (category 4)** —
    `lean_rs::module::{LeanLibrary, LeanModule, LeanExported, LeanIo,
    LeanArgs, DecodeCallResult, LeanAbi}` for the typed exported-function
    loader; `lean_rs_host::meta::{LeanMetaService, LeanMetaResponse,
    LeanMetaOptions, LeanMetaTransparency, MetaCallStatus, infer_type,
    whnf, heartbeat_burn}` for the optional bounded `MetaM` capability.
    These intentionally stay at sub-module paths per the prompt-18
    Decision 1 (different layer ⇒ different abstraction; see
    `04-host-stack.md`'s "Specialized sub-module surfaces" section).

## Demotions

**No public items were demoted to `pub(crate)` during this audit.**

The three parallel Explore audits (one per crate) inventoried every public
item against the classification above and found no accidentally-public items.
All `pub` types either appear in a method signature on a curated type, are
re-exported at the crate root, or are sealed-trait constructors that have to
be public for sealing to take effect (`LeanArgs`, `DecodeCallResult`,
`LeanAbi`). The audit also confirmed no `pub` field exposure on any struct;
every payload struct (`LeanException`, `HostFailure`, `LeanElabFailure`,
`ProofSummary`, `SessionStats`, `PoolStats`, …) keeps its fields private,
which is what makes the bounded-message and bounded-summary invariants
structural rather than convention.

## Red-flag review

This section records the audit's verdict on the eight standard
deep-module-design red flags. Each row names the flag, gives the verdict,
and cites the evidence.

### Shallow module — none

`lean-rs-sys` modules split ~100 symbols across 12 files (`array`,
`closure`, `consts`, `ctor`, `external`, `io`, `init`, `nat_int`, `object`,
`refcount`, `scalar`, `string`, `types`) — each carries enough hand-rolled
refcount mirrors and constructor invariants to be its own concept, not a
file-per-symbol split. `lean-rs`'s public modules (`error`, `host`,
`module`) each own a distinct domain (typed error boundary; capability
loading and session dispatch; typed exported-function loading); each is
deep enough that callers cannot bypass it (e.g. `LeanSession` is the only
way to invoke a typed capability call, not a transparent wrapper over
`LeanModule::exported`).

### Pass-through wrapper — none

The three potential pass-through candidates each add real transformation:

- `LeanCapabilities` does up to 14 `dlsym` calls at load time and caches
  the resulting `*mut c_void` addresses in `SessionSymbols`. Per-call cost
  drops from one `dlsym` per call to one struct-field read — a real
  amortisation point, not a renamed `LeanLibrary`.
- `LeanHost` resolves `.lake/build/lib/lib{escaped_package}_{lib_name}.{dylib,so}`
  and the matching `.olean` search path from the Lake project root,
  hiding Lake's escape rules (`_` → `__` on the package) and on-disk
  layout from the caller.
- `PooledSession` rewraps a bare `Obj<'lean>` environment under a fresh
  capability borrow at acquire time and returns it to the pool on `Drop`;
  it is not a thin alias for `LeanSession`.

### Temporal decomposition — none, by deliberate design

`RD-2026-05-17-006` collapsed the prompt-10 prescribed
`Init` / `Link` / `Load` / `Conversion` / `Internal` error variants into a
single `LeanError::Host(HostFailure { stage: HostStage })` shape. The
collapse was explicit recognition that those five variants were lifecycle
stages of one concern (host setup) rather than five independent failure
classes — the canonical temporal-decomposition smell, called out in
Ousterhout ch 5.3. The `LeanDiagnosticCode` projection layered on top (per
the OBSERVABILITY-DIAGNOSTICS contract) gives callers a stable
caller-facing taxonomy without re-introducing per-stage variants.

### Information leakage — none

- Per-call C ABI shape (unboxed scalar vs. boxed `lean_object *`, IO-wrap
  vs. pure return) is hidden behind the sealed `LeanAbi` /
  `DecodeCallResult` traits inside `lean_rs::module`. Callers write
  `module.exported::<(u32, String), LeanIo<u64>>(name)` and never see
  Lake's per-type representation choice.
- The `'lean` cascade hides runtime ownership: callers never see `lean_inc`,
  `lean_dec`, or even the existence of `Obj<'lean>` (`pub(crate)`). The
  cascade alone enforces "no handle outlives the runtime borrow" without
  any caller-facing lifetime annotation discipline.
- `Lean.Environment` is private to `LeanSession`; every semantic query
  goes through a Lean-authored capability export. CLAUDE.md's "Lean owns
  elaboration / kernel checking / environment" is honoured by the API
  shape, not just by convention.

### Special-general mixture — none

The bounded `MetaM` capability is the test case. Three of the fourteen
`SessionSymbols` addresses are optional (`meta_infer_type`, `meta_whnf`,
`meta_heartbeat_burn`); the only call site that touches them is
`LeanSession::run_meta`. Promoting the meta surface to the crate root
would mix one optional capability with thirteen mandatory ones in the
same namespace. The prompt-18 Decision 1 keeps the meta types at
`lean_rs_host::meta::*` instead — callers opt in via
`use lean_rs_host::meta::{...}` only when they need it. Rationale is
recorded in `docs/architecture/04-host-stack.md` ("Specialized sub-module
surfaces").

### Conjoined methods — none

The candidate is the kernel-check / evidence pair. The audit verified the
split is deliberate:

- `LeanSession::kernel_check` does parse + elaborate + kernel-check and
  returns a `LeanKernelOutcome` carrying a `LeanEvidence` handle.
- `LeanSession::summarize_evidence` does the pretty-print projection on
  demand.
- `LeanSession::check_evidence` does re-validation on demand.

Eagerly bundling `ProofSummary` into the `Checked` variant would force
every `kernel_check` caller to pay the pretty-print cost (non-trivial for
realistic `Lean.Expr` values), even though most callers only inspect the
`EvidenceStatus` tag. The split keeps the cheap path cheap and lets the
expensive paths be paid only when the caller asks. The
`docs/architecture/04-host-stack.md` "Methods on the curated types" section
pins this shape.

### Hard-to-describe API — none

The curated surface fits in one classification table
(`docs/architecture/04-host-stack.md` §"Classification table") and reduces
to one sentence at a call site: **runtime → host → capabilities → session
→ typed query**. The five-line happy-path snippet now in the
`crate::lean_rs` root doc exercises every entry-point promotion.
Specialized capabilities (`module`, `host::meta`) have one-line
descriptions of when to reach for them.

### Implementation details contaminating comments — fixed

The audit found "lands in prompt N" / "follows in prompt N" / "scheduled by
prompt N" residue across `lib.rs`, `host/mod.rs`, `host/capabilities.rs`,
`host/pool.rs`, `host/session.rs`, `host/evidence/{mod,handle,status}.rs`,
`host/elaboration/mod.rs`, `module/mod.rs`, `module/loaded.rs`,
`runtime/mod.rs`, `runtime/obj.rs`, `runtime/init.rs`, `abi/mod.rs`,
`abi/scalar.rs`, `abi/bytearray.rs`, `abi/except.rs`, `error/io.rs`,
`error/panic.rs`. Every occurrence was rewritten into present-tense,
implementation-agnostic prose during this audit; the verification command
`rg -nE "(land(s|ed|ing)|follow(s|ed)|scheduled).*prompt [0-9]" crates/`
returns no matches. The lone surviving "lands in" is a literal value /
field usage in `lean-rs-sys/src/ctor.rs:132` ("when a `UInt32` value lands
in a constructor field"), not staleness.

## Documentation verdicts

- **Crate-level docs.** All three crates open with a focused mission
  statement; `lean-rs`'s doc opens with the 5-line happy-path cascade
  (`rust,ignore`) that exercises the curated entry points end to end.
  `lean-toolchain`'s doc names its role and links to `lean-rs-sys` as
  the raw-FFI source. `lean-rs-sys`'s doc describes the FFI contract,
  opaque-type guarantee, layering, and minimum-unsafe discipline.
- **`# Errors` sections.** Every public fallible function returning
  `LeanResult` carries an `# Errors` section naming the failure modes
  (verified by spot-check across `LeanHost`, `LeanCapabilities`,
  `LeanSession::*`, `LeanLibrary`, `LeanModule::exported`,
  `LeanExported::call`, `SessionPool::acquire`).
- **`# Panics` sections.** No public function in `lean-rs` or
  `lean-toolchain` can panic in the absence of a documented internal
  invariant violation; correspondingly no `# Panics` section exists. The
  workspace-wide `unsafe-code = "deny"` lint plus the clippy
  `panic`/`unwrap_used`/`expect_used` checks structurally enforce this.
- **`# Safety` sections in `lean-rs-sys`.** Every `pub unsafe fn` (99 of
  them across `array`, `closure`, `ctor`, `external`, `io`, `object`,
  `refcount`, `scalar`, `string`) carries a substantive `# Safety` section
  naming the precondition (e.g. "borrowed Lean array; valid for
  `lean_array_capacity(o)` elements"). No section uses the placeholder
  pattern (`"see lean.h"`, `"uphold all Lean invariants"`). The lint at
  `crates/lean-rs-sys/tests/safety_grep.rs` enforces presence; this audit
  re-read wording and found zero offenders.
- **Intra-doc links.** Crate-root references use bare names
  (`[`LeanRuntime`]`); cross-module references use `crate::`-qualified
  paths (`[`crate::host::meta::LeanMetaService`]`). `RUSTDOCFLAGS="-D
  warnings" cargo doc --no-deps --workspace --document-private-items` is
  clean.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items
# baselines exist and match the live surface
test -f docs/api-review/lean-rs-sys-public.txt
test -f docs/api-review/lean-toolchain-public.txt
test -f docs/api-review/lean-rs-public.txt
# every curated re-export name shows up in the lean-rs baseline
diff -q \
    <(rg -oN 'use crate::[^;]*::[A-Z_a-z0-9]+' crates/lean-rs/src/lib.rs | sed 's|.*::||' | sort -u) \
    <(grep -oE 'lean_rs::[A-Z_a-z0-9]+' docs/api-review/lean-rs-public.txt | sed 's|lean_rs::||' | sort -u | comm -12 - <(rg -oN 'use crate::[^;]*::[A-Z_a-z0-9]+' crates/lean-rs/src/lib.rs | sed 's|.*::||' | sort -u))
```

## Out of scope (deferred to prompt 28)

- Tagging v0.1 of `lean-toolchain` and `lean-rs`.
- crates.io publication ordering (`lean-rs-sys` first, then
  `lean-toolchain`, then `lean-rs`).
- Downstream-integration smoke (a real Rust application depending on
  published `lean-rs`).
