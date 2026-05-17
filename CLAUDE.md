# lean-rs

A Rust binding stack for hosting Lean 4 capabilities. Lean owns elaboration, kernel checking, proof objects,
universes, `MetaM`, and dependent-type meaning; this project owns linking, runtime initialization, ABI conversion,
module loading, error and panic boundaries, scheduling, diagnostics, batching, and packaging.

Work in this repo is driven by a prompt sequence, not ad hoc tasks. Read the next session's prompt and the live
contract state before writing code.

## Read first, every session

1. `/Users/jcreinhold/Code/prompts/lean-rs/00-current-state.md` — the source of truth after prompt 01. Use the
   actual names, paths, and caveats recorded there, not the preferred designs in prompt files.
2. `/Users/jcreinhold/Code/prompts/lean-rs/00-recovery-protocol.md` — what to do when a prompt's assumptions
   collide with reality. Stop and emit a Replanning Delta; do not paper over with brittle wrappers.
3. The current prompt file (`prompts/lean-rs/NN-*.md`).

## Workspace shape

Two published crates plus two workspace-internal helpers:

| Crate                    | Role                                                                                    |
| ------------------------ | --------------------------------------------------------------------------------------- |
| `lean-rs-sys`            | In-tree raw Lean 4 C ABI bindings, signature-checked symbol allowlist, header digest, link directives (`publish = false`). |
| `lean-toolchain`         | Toolchain discovery, typed fingerprint, fixture digest, link diagnostics, build helpers. |
| `lean-rs`                | Single safe front door. Modules: `runtime`, `abi`, `module`, `host`, `batch`, `error`. |
| `lean-rs-test-support`   | Internal fixtures (`publish = false`).                                                  |

Layering: `lean-rs-sys` → `lean-toolchain` → `lean-rs`. Raw `lean_*` symbols enter only through `lean-rs-sys` and
live in `pub(crate)` modules of `lean-rs`; safe APIs never re-export them.

## Build and verify

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

CI runs the same four commands on `ubuntu-latest` and `macos-latest`, stable Rust only.

## Discipline

- **Raw `lean_*` symbols enter the workspace only via `lean-rs-sys`.** If a symbol is missing or has a different
  signature in the active Lean header, extend the extern declarations and the allowlist in `lean-rs-sys` and record
  the version delta under `VERSION-COMPATIBILITY`. Stop with a Replanning Delta if ownership conventions or
  layout assumptions shift.
- **No broad `pub use` facade.** Re-exports at `lean_rs::*` are curated public API, not path-shortening.
- **No speculative traits with one implementor.** Add a trait when a second concrete type needs it.
- **No `unwrap()`, `expect()`, or `panic!`** in non-test code unless a comment names a proof obligation.
- **`unsafe-code = "deny"`** at workspace level. `lean-rs-sys` is the one crate-wide opt-out; new `unsafe`
  elsewhere needs justification, a `// SAFETY:` comment naming the invariant, and reviewer sign-off.
- **No legacy `lean4-*` crate names.** They were collapsed into `lean-rs` modules during bootstrap; references in
  prompt files are historical record (see `RD-2026-05-17` in `00-current-state.md`).
- **No external `lean-sys` dependency.** The original adoption was reverted by `RD-2026-05-17-003`; raw FFI lives
  in the in-tree `lean-rs-sys` crate.
- **No `TODO`, `unimplemented!()`, `todo!()`.** Build the intended functionality or stop.
- **Fix bugs at their root.** If the cause lives in a different module, fix it there.
- **Update `00-current-state.md`** before finishing any prompt that changes the implementation repo.

## When CLAUDE.md is wrong

This file should drift slowly. If a session reveals something here is stale, fix it in the same PR — do not add a
note saying it's stale. The same rule applies to prompt files, contract claims, and architecture docs.
