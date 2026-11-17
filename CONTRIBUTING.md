# Contributing to lean-rs

Use the [architecture map](docs/architecture/README.md) to find the owning contract. Read the
[charter](docs/architecture/charter.md), [safety model](docs/architecture/safety.md), and the owning topic before changing
a boundary. Repository coding invariants live in [AGENTS.md](AGENTS.md); keep the owning documentation current when
behavior changes.

## Build and verify

The workspace Rust gates are:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

Use nextest for the full suite: separate test processes bound cumulative Lean state. See [Testing](docs/testing.md) for
fixture setup, resource limits, Linux unwind settings, and focused debugging commands.

[CI](.github/workflows/ci.yml) normally checks Ubuntu on the head Lean toolchain. Manual `full_matrix` checks expand to
the supported window on Ubuntu and macOS; [release verification](.github/workflows/release.yml) checks the head on both
systems. CI also checks docs.rs builds, packages, and committed API baselines. The full toolchain policy and current
coverage are in [Version matrix](docs/version-matrix.md).

Documentation has a separate [build workflow](.github/workflows/docs.yml). For prose-only changes, build the book and
check affected local links, anchors, commands, and source claims:

```sh
mdbook build docs/
git diff --check
```

Do not regenerate API baselines to hide an unintended API change. Follow [Public-API review](docs/api-review.md) when an
intentional change needs a new baseline.

## Unsafe changes

Workspace Rust lints deny unsafe code by default. `lean-rs-sys` owns raw Lean symbols and opaque public FFI types; other
crates opt out only at justified narrow boundaries.

A change that adds or reshapes unsafe code needs:

1. An allow attribute on the smallest suitable scope, justified in the change description.
1. A `// SAFETY:` comment on each block naming its invariant and a `# Safety` section on each public unsafe function.
1. An update to the [unsafe inventory](docs/safety/unsafe-inventory.md) and practical checks for plausible violations
   (focused tests, refcount stress, Miri, sanitizers, or fuzzing as appropriate).
1. Sign-off from a reviewer other than the author. Do not self-merge new unsafe code.

Safe callers must not inherit manual Lean reference-counting or pointer-validity obligations. See the safety model for
checked dynamic dispatch, callback lifetimes, and the limits of the guarantee.

## Compatibility and changes

The supported Lean ABI window is defined in [crates/lean-rs-abi/src/supported.rs](crates/lean-rs-abi/src/supported.rs).
Follow [Bump toolchain](docs/bump-toolchain.md) when extending it. Changes to Lean layout, ownership, or initializer
protocol need an explicit compatibility decision before implementation.

Keep commits scoped to one logical change, explain why it is needed, and record relevant validation. Do not amend a
shared commit except to resolve a hook failure on an unshared branch. Report issues with the Lean version, OS, Rust
version, and exact failing command.
