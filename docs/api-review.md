# Public API Review

[`api-review/*-public.txt`](https://github.com/jcreinhold/lean-rs/tree/main/docs/api-review) contains `cargo public-api --simplified` baselines for the eight crates
currently gated by [CI](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/ci.yml). Intentional API changes update the matching baseline in the same
commit. `lean-rs-abi` currently has no committed baseline or CI comparison.

## Regenerate

Use the pinned pair: `cargo-public-api@0.52.0` and `nightly-2026-06-18`. Nightly rustdoc output, including derived
generic bounds, varies with the toolchain. Update both pins consistently in `ci.yml`, `release.yml`, and
[`scripts/prerelease.sh`](https://github.com/jcreinhold/lean-rs/blob/main/scripts/prerelease.sh), then regenerate all baselines when either changes.

```sh
cargo install cargo-public-api@0.52.0 --locked
rustup toolchain install nightly-2026-06-18 --profile minimal
```

From the repository root, generate each baseline into a temporary file first so a failed build cannot replace it with
partial output. Keep stderr visible for build failures:

```sh
set -eu
actual=$(mktemp)
trap 'rm -f "$actual"' EXIT
for crate in lean-rs-sys lean-toolchain lean-rs lean-rs-host \
             lean-rs-worker-protocol lean-rs-worker-parent lean-rs-worker-child; do
  cargo +nightly-2026-06-18 public-api --simplified -p "$crate" > "$actual"
  cp "$actual" "docs/api-review/${crate}-public.txt"
done
```

## Review the change first

Check that the new API owns a coherent responsibility, hides ABI and lifecycle details, and lets callers pay only for
the work they request. Avoid pass-through wrappers, speculative traits, broad crate-root re-exports, and separate error
variants for internal stages of one failure. A reader should be able to explain the entry point and use it in a short
example. Comments describe current invariants rather than implementation history.

Fallible public functions document their errors. Every public unsafe function documents concrete safety preconditions;
"see lean.h" is insufficient. Use resolvable rustdoc links and run the [workspace gates](https://github.com/jcreinhold/lean-rs/blob/main/CONTRIBUTING.md), including
`cargo nextest run --workspace` and doctests. Broad same-process `cargo test` is not the workspace test gate; see
[testing](testing.md). For a review that changes private documentation, also check:

```sh
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items
```
