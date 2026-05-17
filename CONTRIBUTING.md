# Contributing to lean-rs

Thanks for your interest. This file documents the rules a contribution must satisfy. Read it before opening a PR.

## Build and verify

Every change must pass, on Linux and macOS:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo doc --no-deps
```

CI runs the same commands. If a command fails locally it will fail in CI.

## Unsafe code

`unsafe-code = "deny"` at the workspace level. Raw Lean 4 C ABI declarations enter the project only through the
external [`lean-sys`](https://crates.io/crates/lean-sys) crate (digama0/Mario Carneiro); we do not maintain our own
raw FFI crate. `unsafe` blocks inside `lean-rs` exist only to call into `lean-sys` and to bridge to safe wrappers
(`LeanRuntime`, `LeanObj`, etc.).

Adding `unsafe` anywhere in the workspace requires:

1. A `#![allow(unsafe_code)]` (or narrower) attribute on the smallest possible scope, justified in the PR description.
2. A `// SAFETY:` comment on every `unsafe { ... }` block naming the invariant the caller (or context) is relying on.
3. Tests that would fail under a plausible violation of that invariant when practical (Miri, sanitizer, refcount
   stress, or focused unit tests on the unsafe boundary).
4. Reviewer sign-off from someone other than the author. Self-merging a new `unsafe` block is not allowed.

The safe APIs in `lean-rs` must not require callers to know Lean reference-counting conventions. Leaking an unsafe
precondition into a safe function's contract is a contract change, not an implementation detail.

## Lean-version compatibility

This project tracks a specific Lean toolchain and ABI. Bumping the supported Lean version is a compatibility decision,
not a build fix:

- Record the supported Lean version range in `/Users/jcreinhold/Code/prompts/lean-rs/00-current-state.md` under the
  `VERSION-COMPATIBILITY` contract (added in prompt 02; until then, note compatibility in the relevant prompt's
  current-state entry).
- Add a CI matrix entry or a documented build flag covering the new version before claiming support.
- A change in Lean's C ABI (object layout, ownership convention, initializer protocol) is a contract change. Follow
  `/Users/jcreinhold/Code/prompts/lean-rs/00-recovery-protocol.md` and stop with a Replanning Delta rather than
  papering over the difference with brittle wrappers.

## Coding standards

- No `unwrap()`, `expect()`, or `panic!` in non-test code unless the surrounding comment names a proof obligation.
- No public re-exports purely to shorten import paths. Canonical import paths only.
- No speculative traits with a single implementor. Add a trait when a second concrete type needs it.
- No safe wrapper that forwards a raw C ABI call without hiding ownership, initialization, error, or versioning
  complexity.
- Comments explain caller-visible invariants or non-obvious rationale. They do not narrate code.
- Prefer editing existing files over creating new ones. Do not add `TODO`, `unimplemented!()`, or `todo!()` to make
  code compile.

## Commits

- One logical change per commit. Commit messages explain why, not what.
- Reference the relevant contract id or prompt number from the implementation sequence when applicable.
- Do not amend a commit that has already been pushed except to fix a hook failure on a not-yet-shared branch.

## Reporting issues

Use the GitHub issue tracker. Include the Lean toolchain version, host OS, Rust toolchain version, and the exact
command that failed.
