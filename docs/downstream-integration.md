# Downstream Integration Guide

What an external Rust application needs to know to depend on the published `lean-rs` /
`lean-rs-host` crates. Two external proof apps exercise the full integration end to end and
serve as worked examples:

- `lean-rs-downstream` — L1 only, depends on `lean-rs` and exercises the `LeanRuntime` → `LeanLibrary` → `LeanModule` → `LeanExported<Args, R>::call` cascade against the consumer's own `@[export]` Lean declarations. No `lean-rs-host` node in the dependency graph.
- `lean-rs-host-downstream` — L2, depends on both `lean-rs` and `lean-rs-host`, adds the shim `require` to its `lakefile.lean`, and drives `LeanHost::from_lake_project` → `LeanCapabilities::load_capabilities` → `LeanSession` (`query_declaration`, `kernel_check`, `summarize_evidence`, `call_capability`).

The L1 proof confirms the typed-FFI primitive surface drives a consumer-authored Lake-built
Lean library end to end with zero `lean_rs_host_*` shims involved. The L2 proof exercises the
hybrid shim-package layout below.

## Hybrid shim-package layout (L2 consumers)

`lean-rs-host`'s 13 mandatory + 3 optional `lean_rs_host_*` `@[export]` shims (557 LOC of
Lean reaching into `Lean.Elab.Frontend`, the kernel checker, and `MetaM`) are an out-of-Rust
contract: an L2 consumer must have the shim symbols loadable into the same process to satisfy
`LeanCapabilities::load_capabilities`. The shipping shape is a hybrid: source organisation as
a separate Lake package, runtime load as two dylibs sharing the dynamic linker's global
namespace.

The Lake-level shape `LeanLib.sharedFacet` does *not* transitively bundle a required
package's `@[export]` symbols into the consumer's dylib. `nm` confirms zero `lean_rs_host_*`
symbols in a consumer dylib built against the shim as a path-require; all 16 end up in the
shim package's separate `liblean__rs__host__shims_LeanRsHostShims.dylib`. The mechanism that
works:

- **`lake/lean-rs-host-shims/`** is the Lake package shipping the three contract files (`Environment.lean`, `Elaboration.lean`, `Meta.lean`) as `LeanRsHostShims.*`, with its own `lakefile.lean`, `lean-toolchain` pin, and `lean_lib LeanRsHostShims where defaultFacets := #[LeanLib.sharedFacet]`. Consumers add `require lean_rs_host_shims from "…"` (path or git) to their `lakefile.lean`; `lake build` produces both dylibs.
- **`LeanCapabilities::load_capabilities`** opens both dylibs. The shim dylib is opened **first** via `LeanLibrary::open_globally` (`RTLD_LAZY | RTLD_GLOBAL` on Unix) so the consumer's transitive reference to `_initialize_lean__rs__host__shims_LeanRsHostShims` resolves through the dynamic linker's global namespace. Without `RTLD_GLOBAL` the consumer's initializer chain SIGSEGVs jumping to the unresolved symbol.
- **`LakeProject::shim_dylib()` and `shim_olean_search_path()`** read the consumer's `lake-manifest.json` (via `serde_json`) and resolve the shim package's on-disk locations. Both `type: "path"` and `type: "git"` require entries are handled.
- **`lean_rs_host_session_import`** takes `(searchPaths : Array String) (importNames : Array String)` so the consumer's `.olean` directory and the shim package's `.olean` directory both appear on the search path for `Lean.importModules`.

A note on what stays out of scope: `fixtures/lean/LeanRsFixture.lean` does **not**
`import LeanRsHostShims`. If it did, the fixture's compiled dylib would carry a static
dependency on the shim's `initialize_*` symbols, and the L1 tests in
`crates/lean-rs/src/{module,handle}/tests.rs` (which open the fixture dylib directly via
`LeanLibrary::open` without `LeanCapabilities` orchestration) would SIGSEGV on the unresolved
symbol. The L1 fixture stays shim-independent at link time; the L2 tests reach shim modules
at runtime by naming them in `caps.session(&["LeanRsHostShims.…"])`.

## Caveats every consumer hits

### Downstream `build.rs` needs to re-emit the Lean rpath

`lean-rs-sys`'s `build.rs` emits `cargo:rustc-link-arg=-Wl,-rpath,...` for the active Lean
toolchain's `lib/lean` directory, but `cargo:rustc-link-arg` does not propagate to dependent
crates. `lean-rs` and `lean-rs-host` each carry their own `build.rs` with the same
Lean-discovery + rpath logic. The downstream app needs the same.

`lean-rs-downstream/build.rs` re-implements the ~30-line Lean-prefix discovery + rpath
emission. It works but is a copy-paste of `lean-rs/build.rs`. A `lean_toolchain::emit_rpath()`
build-script helper would let downstream `build.rs` scripts shrink to a two-line `fn main()`;
see *Open issues* below.

### Nullary unboxed-scalar globals trip the function-path dispatch

Declaring a nullary `@[export]` that returns an unboxed scalar — e.g.,
`@[export downstream_app_decide_true] def decideTrue : Bool := decide (1 + 1 = 2)` — produces
a Lake-compiled persistent global (the standard optimisation for nullary constants).
`LeanModule::exported`'s function-path dispatch then reads the global's stored scalar-tagged
value as if it were an aligned function pointer; `LeanExported::call` panics with
`misaligned pointer dereference: address must be a multiple of 0x8 but is 0x104954031` — a
scalar-boxed `Bool` value being treated as a pointer.

**Workaround:** add a `Unit` argument so Lake emits a function symbol:

```lean
def decideTrue (_ : Unit) : Bool := decide (1 + 1 = 2)
```

The Rust call site then spells the typed shape as
`module.exported::<((),), bool>(...).call(())`. See *Open issues* below for the planned
lookup-time diagnostic.

### Pre-publish `lean-rs = "0.1"` requires a path dep

Before `lean-rs 0.1.0` lands on crates.io, the downstream's `Cargo.toml` pins both
`version = "0.1"` and `path = "../lean-rs/crates/lean-rs"`. Cargo enforces the version
constraint at build time; once `lean-rs 0.1.0` is published, drop the `path =` attribute.

## What was confirmed not needed

- **No widening of the typed-handle generics.** `LeanExported<Args, R>::call` worked end-to-end against unboxed-scalar (`(u64, u64) → u64`), nullary `Unit` arg (`((),) → bool`), and the host crate's bulk-method shapes without any extension.
- **No `lean-rs-sys` dependency in the downstream.** `cargo tree -e normal` from the downstream shows `lean-rs-sys` only transitively through `lean-rs`. The opt-in raw FFI escape hatch did not need to be reached.
- **No `lean-rs-host` dependency in the L1 downstream.** Embedders who *want* Lean prelude access on top of their own modules add `lean-rs-host` and a `LeanSession`; embedders who don't aren't paying for it.
- **No `Lean.importModules` orchestration in the L1 downstream.** It uses `LeanLibrary::initialize_module` (the L1 primitive) directly; there is no host-managed session in scope.

## Open issues

- A `lean_toolchain::emit_rpath()` build-script helper that lets a downstream `build.rs` shrink to a two-line `fn main()`. Closes the rpath copy-paste caveat above.
- A `LeanModule::exported` change that handles nullary unboxed-scalar globals cleanly: either (a) detect at lookup time and return a `Linking` diagnostic naming the `Unit`-argument workaround, or (b) provide a separate typed-global accessor that handles the scalar-tagged-pointer decode.
- Promote `lake/lean-rs-host-shims/` to a public Lake registry entry (Reservoir or the GitHub Lake registry) so external consumers can write `require lean_rs_host_shims @ "0.1"` without a git URL. The hybrid layout works today with `from git "…" @ "v0.1.0"` once `lean-rs` is tagged.
- v1.0 compatibility-promise scope split per crate.

## Re-run the verification

```sh
# main repo (must remain green)
cd /Users/jcreinhold/Code/lean-rs
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

# L1 downstream (uses lean-rs only; no lean-rs-host)
cd /Users/jcreinhold/Code/lean-rs-downstream
(cd lean && lake build)
cargo build && cargo test && cargo run
cargo tree -e normal | head -3                                  # no lean-rs-host
! grep -nE 'lean_rs_host|LeanHost|LeanSession|LeanCapabilities|SessionPool' \
    src/*.rs tests/*.rs

# L2 downstream (uses both; exercises hybrid layout)
cd /Users/jcreinhold/Code/lean-rs-host-downstream
(cd lean && lake build)
cargo build && cargo test && cargo run
cargo tree -e normal | head -5                                  # lean-rs-host present
```

All commands pass on macOS arm64 (Lean 4.29.1, Rust 1.95.0 stable).
