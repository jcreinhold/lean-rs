# Public-API Review Framework

How the four published crates' public surfaces are organised, and how to audit them when the
surface changes. Used as the pre-flight check before any release.

## Baselines

`cargo public-api --simplified` baselines are committed under
[`docs/api-review/`](api-review/):

| Crate | Baseline | Lines |
| --- | --- | ---: |
| `lean-rs-sys` | [`lean-rs-sys-public.txt`](api-review/lean-rs-sys-public.txt) | 259 |
| `lean-toolchain` | [`lean-toolchain-public.txt`](api-review/lean-toolchain-public.txt) | 124 |
| `lean-rs` | [`lean-rs-public.txt`](api-review/lean-rs-public.txt) | 1,159 |
| `lean-rs-host` | [`lean-rs-host-public.txt`](api-review/lean-rs-host-public.txt) | 899 |

PRs that touch the public surface diff against these files; intentional changes regenerate the
baseline in the same commit. The `--simplified` flag strips blanket auto-trait impls so the
diff stays signal-heavy.

Regenerate:

```sh
for c in lean-rs-sys lean-toolchain lean-rs lean-rs-host; do
  cargo public-api -p "$c" --simplified > "docs/api-review/${c}-public.txt"
done
```

## Classification

Every public item maps to one of five categories:

1. **Raw FFI entry point** (`lean-rs-sys` only) — opaque `lean_object` plus `pub unsafe fn` helpers; every item carries a `# Safety` section.
2. **Toolchain metadata / build helper** (`lean-toolchain` only).
3. **Curated entry point** at a crate root — items promoted by the `pub use` blocks at the crate root.
4. **Module-level escape hatch** at a sub-module path — visible to power users, not promoted to the crate root.
5. **Internal-but-accidentally-public** — items that should be `pub(crate)`.

The per-crate placement is documented inline:
[`docs/architecture/04-host-stack.md`](architecture/04-host-stack.md) for the L2 surface; the
`docs/api-review/*-public.txt` baselines for the rest. Today every `pub` item maps to
category 1, 2, 3, or 4; category 5 should always be empty after a review.

## Red-flag checklist

Eight deep-module-design smells. Each is a "none in the current surface" verdict the
reviewer reconfirms; the example after each is what to look for.

**Shallow module.** Does each module own enough state and behaviour to be its own concept,
or is it a file-per-symbol split? `lean-rs-sys` splits ~100 symbols across 12 files (`array`,
`closure`, `consts`, `ctor`, `external`, `io`, `init`, `nat_int`, `object`, `refcount`,
`scalar`, `string`, `types`); each is a category, not a single symbol. In `lean-rs`,
`LeanSession` is the only way to invoke a typed capability call — not a transparent wrapper
over `LeanModule::exported`.

**Pass-through wrapper.** Does the wrapper add transformation, or just rename? Three current
candidates each add real work:

- `LeanCapabilities` does up to 14 `dlsym` calls at load time and caches the addresses in `SessionSymbols`; per-call cost drops from one `dlsym` to one field read.
- `LeanHost` resolves `.lake/build/lib/lib{escaped_package}_{lib_name}.{dylib,so}` plus the `.olean` search path, hiding Lake's escape rules (`_` → `__` on the package) and on-disk layout.
- `PooledSession` rewraps a bare `Obj<'lean>` environment under a fresh capability borrow at acquire time and returns it on `Drop`.

**Temporal decomposition.** Are error variants modelling lifecycle stages rather than
independent failure classes? The host-failure path collapses init / link / load / conversion
/ internal into a single `LeanError::Host(HostFailure { stage: HostStage })` — five
lifecycle stages of one concern (host setup), not five independent failure classes (Ousterhout
ch. 5.3). The `LeanDiagnosticCode` projection gives callers a stable caller-facing taxonomy
without re-introducing per-stage variants.

**Information leakage.** Does the per-call C ABI shape (unboxed scalar vs boxed
`lean_object *`, IO-wrap vs pure return) leak into the caller's types? It's hidden behind the
sealed `LeanAbi` / `DecodeCallResult` traits; callers write
`module.exported::<(u32, String), LeanIo<u64>>(name)` and never see Lake's per-type
representation choice. The `'lean` cascade hides runtime ownership; `Lean.Environment` is
private to `LeanSession`, with every semantic query routed through a Lean-authored capability
export.

**Special-general mixture.** Are optional capabilities mixed into the mandatory crate root?
Test case: the bounded `MetaM` capability. Three of the fourteen `SessionSymbols` addresses
are optional (`meta_infer_type`, `meta_whnf`, `meta_heartbeat_burn`); only
`LeanSession::run_meta` touches them. The meta types stay at `lean_rs_host::meta::*` so
callers opt in only when they need them.

**Conjoined methods.** Does a single method bundle two operations callers should be able to
choose between? The kernel-check / evidence split is deliberate. `LeanSession::kernel_check`
parses + elaborates + checks and returns a `LeanKernelOutcome` carrying a `LeanEvidence`
handle; `summarize_evidence` and `check_evidence` are on-demand. Eagerly bundling
`ProofSummary` into the `Checked` variant would force every `kernel_check` caller to pay the
pretty-print cost — non-trivial for realistic `Lean.Expr` values — even though most callers
only inspect the `EvidenceStatus` tag.

**Hard-to-describe API.** Can a new reader reduce the surface to a single sentence? Yes:
**runtime → host → capabilities → session → typed query.** The five-line happy-path snippet
in the `lean_rs` root doc exercises every entry-point promotion; the curated surface fits in
one classification table ([`04-host-stack.md`](architecture/04-host-stack.md)).

**Implementation details contaminating comments.** Are doc comments naming the work that
produced the code instead of describing the abstraction? `rg -nE
"(land(s|ed|ing)|follow(s|ed)|scheduled).*\b(prompt|RD-[0-9])" crates/` should return no
matches. (The lone surviving "lands in" in `lean-rs-sys/src/ctor.rs:132` — "when a `UInt32`
value lands in a constructor field" — is a literal value/field reference, not staleness.)

## Documentation rules

- **Crate-level docs** open with a focused mission statement. `lean-rs`'s doc leads with the 5-line happy-path cascade (`rust,ignore`) that exercises the curated entry points end to end. `lean-toolchain` names its role and links to `lean-rs-sys` as the raw-FFI source. `lean-rs-sys` describes the FFI contract, opaque-type guarantee, layering, and minimum-unsafe discipline.
- **`# Errors` sections.** Every public fallible function returning `LeanResult` carries one naming the failure modes (`LeanHost`, `LeanCapabilities`, `LeanSession::*`, `LeanLibrary`, `LeanModule::exported`, `LeanExported::call`, `SessionPool::acquire` all comply).
- **`# Panics` sections.** No public function in `lean-rs` or `lean-toolchain` can panic in the absence of a documented internal invariant violation; correspondingly no `# Panics` section exists. The workspace-wide `unsafe-code = "deny"` lint plus the clippy `panic` / `unwrap_used` / `expect_used` checks structurally enforce this.
- **`# Safety` sections in `lean-rs-sys`.** Every `pub unsafe fn` (99 across `array`, `closure`, `ctor`, `external`, `io`, `object`, `refcount`, `scalar`, `string`) carries a substantive section naming the precondition (e.g. "borrowed Lean array; valid for `lean_array_capacity(o)` elements"). Placeholder patterns ("see lean.h", "uphold all Lean invariants") are not acceptable. The lint at `crates/lean-rs-sys/tests/safety_grep.rs` enforces presence.
- **Intra-doc links.** Crate-root references use bare names (`[`LeanRuntime`]`); cross-module references use `crate::`-qualified paths (`[`crate::host::meta::LeanMetaService`]`). `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items` must be clean.

## Out of scope

- Changes to `lean-rs-sys`'s extern declarations or `REQUIRED_SYMBOLS` allowlist (go through their own review).
- Sealed-trait constructors that have to be public for the seal to take effect (`LeanArgs`, `DecodeCallResult`, `LeanAbi`) — these stay public; their `# Safety` and trait-bound discipline is reviewed elsewhere.
- Performance or refactoring changes that don't touch the public surface.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items
test -f docs/api-review/lean-rs-sys-public.txt
test -f docs/api-review/lean-toolchain-public.txt
test -f docs/api-review/lean-rs-public.txt
test -f docs/api-review/lean-rs-host-public.txt
```
