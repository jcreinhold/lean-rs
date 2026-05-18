# Downstream integration log

This document records what was learned from building the first external consumer of
`lean-rs` v0.1 — the proof point for prompt 29. The downstream app lives at
`/Users/jcreinhold/Code/lean-rs-downstream/` (not in this workspace; not in the
`crates/lean-rs/Cargo.toml` `[workspace] members` glob). The integration is shaped by
`RD-2026-05-18-001` in [`prompts/lean-rs/00-current-state.md`](../prompts/lean-rs/00-current-state.md).

## What the downstream proves

The L1 typed-FFI primitive surface of `lean-rs` (the `LeanRuntime` →
`LeanLibrary` → `LeanModule` → `LeanExported<Args, R>::call` cascade plus the
`LeanError` / `LeanDiagnosticCode` boundary) is sufficient to drive a downstream-authored
Lake-built Lean library end to end without any reference to the opinionated
`lean-rs-host` theorem-prover-host stack. The downstream's Lean source contains two
`@[export]` declarations of its own design (one pure computation, one semantic check via
Lean's `decide`); zero `lean_rs_host_*` shims are involved.

This is the (β)-binding norm — the same shape every `ocaml-rs` / `hs-bindgen` / `caml-oxide`-style
binding follows. The architectural survey that drove `RD-2026-05-18-001` (recorded under that
RD in `00-current-state.md`) confirmed that no mainstream Rust ↔ GC-language binding ships
pre-compiled target-language helper code; per-application Lean shims are part of how Lean is
meant to be embedded, not a friction to design away.

## Integration gaps surfaced and the fixes that landed

### Gap 1 — L1/L2 conflation in `lean-rs`

**Before.** The opinionated theorem-prover-host stack (`LeanHost`, `LeanCapabilities`,
`LeanSession` + their elaboration / evidence / meta / pool surfaces) shipped in the same
crate as the FFI primitive, behind the same default entry point. Its 13+3 `lean_rs_host_*`
Lean shim contract lived in the test fixture only — no shipping vehicle for external
consumers existed. Prompt 29 could not satisfy its brief (downstream depends on
`lean-rs = "0.1"`) without either copying the fixture shims into the downstream (workspace-private
leak, explicitly forbidden by the prompt) or first packaging them.

**Fix.** `RD-2026-05-18-001` split the workspace into four published crates:

- `lean-rs` is the L1 FFI primitive — `LeanRuntime` + `LeanLibrary` + `LeanModule` +
  `LeanExported` + the typed handles (`LeanName`, `LeanLevel`, `LeanExpr`, `LeanDeclaration`)
  + the structured error boundary. No Lean-side shim contract.
- `lean-rs-host` is the L2 opinionated stack — `LeanHost` / `LeanCapabilities` /
  `LeanSession` / `SessionPool` plus the elaboration / evidence / meta surfaces. Its 13+3
  `lean_rs_host_*` Lean shim contract still lives in `fixtures/lean/` today; packaging it
  as a shipping artifact for external `lean-rs-host` consumers (Lake-require from git
  tag vs. `build.rs`-bundled `liblean_rs_host.{so,dylib}`) is the prompt-30 deliverable.

### Gap 2 — `lean-rs` `pub(crate)` helpers needed by `lean-rs-host`

**Before.** `LeanLibrary::resolve_function_symbol`,
`LeanLibrary::resolve_optional_function_symbol`, and `LeanExported::from_function_address`
were `pub(crate)` — fine while `host` lived inside `lean-rs`, broken once `host` moved to
a sibling crate.

**Fix.** Widened to `pub` at the same call sites; their docstrings now name `lean-rs-host`
as the intended external consumer and document the `unsafe` invariants the
`from_function_address` constructor preserves. External users that follow the documented
contract can drive cached-address typed dispatch themselves, the same way `lean-rs-host`
does.

### Gap 3 — Sealed-trait + ABI-helper visibility for L1 → L2 boundary

**Before.** `LeanAbi` / `IntoLean` / `TryFromLean` were `pub trait`s gated by a `pub(crate)`
`sealed::SealedAbi` marker — closed to all crates including the sibling `lean-rs-host`.
The host stack's structure decoders also reached into `abi::nat`, `abi::structure`, and
`error::bound_message` / `error::LeanError::*` constructors.

**Fix.** Promoted the trait module structure so the sibling can construct sealed impls:

- `lean_rs::abi` is `pub mod`, with `traits`, `nat`, and `structure` as `pub` submodules.
- `lean_rs::abi::traits::sealed` is a `#[doc(hidden)] pub mod` containing `pub trait
  SealedAbi`. The doc-hidden path signals "internal extension point for the
  same-team sibling crate, not public API"; truly external crates that follow the obvious
  warning don't implement it.
- `IntoLean` / `TryFromLean` are promoted from `pub(crate) trait` to `pub trait`.
- `lean_rs::__host_internals` (`#[doc(hidden)] pub mod`) re-exports the constructor
  wrappers (`host_linking`, `host_module_init`, `host_symbol_lookup`,
  `host_module_init_panic`, `host_callback_panic`, `host_internal`, `lean_exception`,
  `bound_message`). The underlying `pub(crate)` constructors on `LeanError` are
  preserved so the RD-2026-05-17-006 bounding invariant (external callers cannot mint
  `LeanError` values arbitrarily) still holds for true external users.

### Gap 4 — Downstream consumers need their own `build.rs` for the Lean rpath

**Before (and after).** `lean-rs-sys`'s `build.rs` emits `cargo:rustc-link-arg=-Wl,-rpath,...`
for the active Lean toolchain's `lib/lean` directory, but `cargo:rustc-link-arg` directives
do not propagate to dependent crates. `lean-rs` and `lean-rs-host` each carry their own
`build.rs` with the same Lean-discovery + rpath logic. **The downstream app needs the same.**

**Workaround landed in the downstream.** `lean-rs-downstream/build.rs` re-implements the
~30-line Lean-prefix discovery + rpath emission. It works but is a copy-paste of
`lean-rs/build.rs`.

**Real fix (deferred).** `lean-toolchain` should expose a one-line build-script helper —
e.g. `lean_toolchain::emit_rpath()` — that downstream `build.rs` scripts invoke from a
two-line `fn main()`. This is genuine integration-friction the survey didn't predict; it
should land alongside the prompt-30 packaging work, ideally before any external user
discovers the rpath caveat on their own.

### Gap 5 — Nullary unboxed-scalar globals fail through the function-path dispatch

**Discovered when.** The downstream's first attempt declared
`@[export downstream_app_decide_true] def decideTrue : Bool := decide (1 + 1 = 2)` — a
*nullary* `Bool`-returning export. Lake compiled it as a persistent global (the standard
optimisation for nullary constants), and `LeanModule::exported`'s function-path dispatch
read the global's stored scalar-tagged value as if it were an aligned function-pointer.
The `LeanExported::call` panicked with `misaligned pointer dereference: address must be a
multiple of 0x8 but is 0x104954031` — a textbook scalar-boxed `Bool` value being treated
as a pointer.

**Workaround.** Adding a `Unit` argument (`def decideTrue (_ : Unit) : Bool := …`) forces
Lake to emit a function symbol, which the typed handle drives correctly. The downstream's
Lean source documents the workaround in a comment, and the Rust call site spells the
typed shape as `module.exported::<((),), bool>(...).call(())`.

**Real fix (potential prompt-30 hardening).** `LeanModule::exported` should either
(a) detect at lookup time that the symbol is a global with unboxed-scalar storage and
return a clear `Linking` diagnostic naming the workaround, or (b) provide a separate
typed-global accessor that handles the scalar-tagged-pointer decode correctly. The
current behavior — panicking on a misaligned pointer dereference deep inside the typed
dispatch — is a poor user experience.

### Gap 6 — Pre-publish `lean-rs = "0.1"` requires a path dep

**Before.** `lean-rs 0.1.0` is package-ready but not on crates.io (per
[`docs/release.md`](release.md) — only the dry-run gate is green at v0.1.0).

**Workaround.** The downstream's `Cargo.toml` pins both `version = "0.1"` and
`path = "../lean-rs/crates/lean-rs"`. Cargo still enforces the version constraint at
build time; once `lean-rs 0.1.0` lands on crates.io (prompt-30 publish), the
`path =` attribute can be dropped and the registry version becomes authoritative.
README.md in the downstream documents the caveat.

## What was *not* needed

- **No widening of the typed-handle generics.** `LeanExported<Args, R>::call` worked
  end-to-end against unboxed-scalar (`(u64, u64) → u64`), nullary `Unit` arg (`((),) → bool`),
  and the host crate's bulk-method shapes without any extension. The `RD-2026-05-17-007`
  one-trait/one-handle design landed cleanly.
- **No `lean-rs-sys` dependency in the downstream.** Confirmed by
  `cargo tree -e normal` from the downstream — `lean-rs-sys` appears only transitively
  through `lean-rs`. The opt-in raw FFI escape hatch did not need to be reached.
- **No `lean-rs-host` dependency in the downstream.** The proof point itself.
- **No `Lean.importModules` orchestration.** The downstream uses
  `LeanLibrary::initialize_module` (the L1 primitive) directly; there is no host-managed
  session in scope. Embedders who *want* Lean prelude access on top of their own modules
  add `lean-rs-host` and a `LeanSession`; embedders who don't aren't paying for it.

## Deferred for prompt 30

- Package the `lean-rs-host` Lean shims (`fixtures/lean/LeanRsFixture/{Environment,
  Elaboration,Meta}.lean`'s 13+3 `lean_rs_host_*` exports) as a shipping artifact
  external `lean-rs-host` consumers can use. Two options on the table per RD-2026-05-18-001:
  - **Option A:** Promote to `lake/lean_rs_host/` as a Lake-require'd package referenced
    by git tag from downstream `lakefile.lean` files.
  - **Option B:** `lean-rs-host`'s `build.rs` runs `lake build` against bundled shim
    sources and emits `liblean_rs_host.{so,dylib}` into `OUT_DIR`; `LeanCapabilities`
    opens both the user dylib and this shim dylib.
- The external-consumer downstream proof for `lean-rs-host` itself (analogous to this
  prompt's L1 proof) — a separate downstream app at `/Users/jcreinhold/Code/lean-rs-host-downstream/`
  exercising the curated L2 surface end to end against the packaged shims.
- The `lean-toolchain::emit_rpath()` build-helper from Gap 4.
- The Lake-global typed-handle dispatch fix from Gap 5 (or a clear linking diagnostic).
- Live `cargo publish` of all four crates in the documented order
  (`lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`).
- v1.0 compatibility-promise scope split per-crate (the prompt-30 hardening pass).

## How to re-run the verification

```sh
# main repo (must remain green)
cd /Users/jcreinhold/Code/lean-rs
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

# downstream app
cd /Users/jcreinhold/Code/lean-rs-downstream
(cd lean && lake build)
cargo build
cargo test
cargo run

# discipline checks
cargo tree -e normal | head -3                                 # no lean-rs-host
! grep -nE 'lean_rs_host|LeanHost|LeanSession|LeanCapabilities|SessionPool' src/*.rs tests/*.rs
```

All commands pass on macOS arm64 (Lean 4.29.1, Rust 1.95.0 stable) at the prompt-29
landing commit.
