# Contributing to lean-rs

Thanks for your interest. This file documents the rules a contribution must satisfy. Read it before opening a PR.

## Build and verify

Every change must pass, on Linux and macOS:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

CI runs the same commands. If a command fails locally it will fail in CI. See [`docs/testing.md`](docs/testing.md) for
why the gate is `cargo nextest`, not `cargo test`.

## Unsafe code

`unsafe-code = "deny"` at the workspace level. Raw Lean 4 C ABI declarations live in the in-tree `lean-rs-sys` crate; it
is the one crate-wide `#[allow(unsafe_code)]` boundary in the workspace. `unsafe` blocks inside `lean-rs` exist only to
call into `lean-rs-sys` and to bridge to safe wrappers (`LeanRuntime`, `pub(crate) runtime::obj::Obj<'lean>`, etc.).

Adding `unsafe` anywhere in the workspace requires:

1. A `#![allow(unsafe_code)]` (or narrower) attribute on the smallest possible scope, justified in the PR description.
1. A `// SAFETY:` comment on every `unsafe { ... }` block naming the invariant the caller (or context) is relying on.
1. Tests that would fail under a plausible violation of that invariant when practical (Miri, sanitizer, refcount stress,
   or focused unit tests on the unsafe boundary).
1. Reviewer sign-off from someone other than the author. Self-merging a new `unsafe` block is not allowed.

The safe APIs in `lean-rs` must not require callers to know Lean reference-counting conventions. Leaking an unsafe
precondition into a safe function's contract is a contract change, not an implementation detail.

See [`docs/architecture/01-safety-model.md`](docs/architecture/01-safety-model.md) for the full thesis and
[`docs/safety/unsafe-inventory.md`](docs/safety/unsafe-inventory.md) for the audit checklist.

## Lean-version compatibility

This project tracks a specific Lean toolchain and ABI. Bumping the supported Lean version is a compatibility decision,
not a build fix:

- The supported window lives in [`crates/lean-rs-sys/src/supported.rs`](crates/lean-rs-sys/src/supported.rs); see
  [`docs/version-matrix.md`](docs/version-matrix.md) for the human-readable mirror and
  [`docs/architecture/02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md) for the
  policy.
- Adding a new release follows the [bump procedure](docs/bump-toolchain.md): add a row to `SUPPORTED_TOOLCHAINS`, add a
  CI matrix entry, run `scripts/test-all-toolchains.sh` locally.
- A change in Lean's C ABI (object layout, ownership convention, initializer protocol) is a contract change. Stop and
  discuss with maintainers before papering over the difference with brittle wrappers.

## Coding standards

- No `unwrap()`, `expect()`, or `panic!` in non-test code unless the surrounding comment names a proof obligation.
- No public re-exports purely to shorten import paths. Canonical import paths only.
- No speculative traits with a single implementor. Add a trait when a second concrete type needs it.
- No safe wrapper that forwards a raw C ABI call without hiding ownership, initialization, error, or versioning
  complexity.
- Comments explain caller-visible invariants or non-obvious rationale. They do not narrate code.
- Prefer editing existing files over creating new ones. Do not add `TODO`, `unimplemented!()`, or `todo!()` to make code
  compile.

## Commits

- One logical change per commit. Commit messages explain why, not what.
- Do not amend a commit that has already been pushed except to fix a hook failure on a not-yet-shared branch.

## Reporting issues

Use the GitHub issue tracker. Include the Lean toolchain version, host OS, Rust toolchain version, and the exact command
that failed.
