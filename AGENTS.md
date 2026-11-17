# lean-rs

Rust bindings for hosting Lean 4. Lean owns elaboration, kernel checking, proof objects, universes, `MetaM`, and
dependent-type meaning. This workspace owns linking, runtime initialization, ABI conversion, loading, error boundaries,
scheduling, diagnostics, batching, and packaging.

Before changing code, read the [architecture charter](docs/architecture/00-charter.md), the
[safety model](docs/architecture/01-safety-model.md), and the owning topic in the
[documentation index](docs/SUMMARY.md).

## Boundaries

- Raw `lean_*` declarations belong only in `lean-rs-sys`. Add symbols in the matching
  `crates/lean-rs-sys/src/<category>.rs`, the `REQUIRED_SYMBOLS` allowlist in `crates/lean-rs-abi/src/symbols.rs`, and
  the probes in `crates/lean-rs-sys/tests/linkage.rs`.
- `lean-rs-abi` is static, link-free metadata: no build script, extern declarations, linker directives, or installed
  toolchain probes. `lean-toolchain` owns discovery and live toolchain identity.
- `lean-rs` owns safe runtime handles, conversions, loading, and callbacks; `lean-rs-host` adds standard Lean services.
  The worker protocol and parent crates do not link `libleanshared`; the worker child does.
- Public FFI types stay opaque. Never expose `LeanObjectRepr` outside `lean-rs-sys` or add public fields to FFI types.
  Safe layers do not re-export raw symbols; callers needing raw FFI depend on `lean-rs-sys` explicitly.
- Keep `lean_rs::*` re-exports curated. Add a trait when a second concrete type needs it.

## Verification

The primary workspace gates are:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

Use nextest for runtime tests: cumulative Lean state can exhaust memory in a shared `cargo test` process. See
[testing.md](docs/testing.md) for focused runs and [.github/workflows/ci.yml](.github/workflows/ci.yml) for the complete
CI matrix and additional checks. Runtime gates use stable Rust on Linux and macOS; API and sanitizer jobs also use
nightly.

## Safety and completion

- Workspace `unsafe_code` is denied. `lean-rs-sys` has the sole crate-wide opt-out. Elsewhere, allow unsafe at the
  smallest scope, justify it, and obtain reviewer sign-off as required by the safety model.
- Every unsafe block needs a `// SAFETY:` comment naming its invariant; every public unsafe function needs a `# Safety`
  section.
- Non-test `unwrap()`, `expect()`, and `panic!` require a comment stating the proof obligation that makes them
  infallible. Do not leave `TODO`, `todo!()`, or `unimplemented!()` placeholders.
- Fix the cause in its owning module. Update the relevant architecture or contract doc when the design changes, and
  correct stale repository guidance in the same change.
