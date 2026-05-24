# Public-API Review

`docs/api-review/*-public.txt` are `cargo public-api --simplified` baselines for the five published crates. CI diffs the
live surface against these files on every PR; intentional changes regenerate the matching baseline in the same commit.

## Regenerate

```sh
for c in lean-rs-sys lean-toolchain lean-rs lean-rs-host lean-rs-worker; do
  cargo public-api -p "$c" --simplified 2>/dev/null > "docs/api-review/${c}-public.txt"
done
```

The `2>/dev/null` matters on a cold target dir: `cargo public-api` triggers a build and the
progress lines would otherwise land in the baseline file and trip the next diff. The prerelease
script's internal regeneration drops stderr the same way.

## Red-flag checklist (review before regenerating)

Walk the diff with these questions. Any "yes" is a stop-and-discuss signal, not necessarily a block.

1. **Shallow module.** Does a new module own enough state and behaviour to be its own concept, or is it a
   file-per-symbol split?
2. **Pass-through wrapper.** Does a new wrapper type add real transformation, or just rename an existing one?
3. **Temporal decomposition.** Do new error variants model lifecycle stages of one concern (Ousterhout ch. 5.3), instead
   of independent failure classes?
4. **Information leakage.** Does per-call C ABI shape (unboxed vs boxed, IO-wrap vs pure) leak into the caller's types?
5. **Special-general mixture.** Are optional or specialised items being mixed into the crate root alongside mandatory
   ones?
6. **Conjoined methods.** Does a single method bundle two operations callers should be able to pay for independently?
7. **Hard-to-describe API.** Can a new reader reduce the surface to one sentence and a five-line snippet?
8. **Implementation details in comments.**
   `rg -nE "(land(s|ed|ing)|follow(s|ed)|scheduled).*\b(prompt|RD-[0-9])" crates/` should return no matches.

## Doc rules

Each `pub` item carries:

- `# Errors` on every fallible function returning `LeanResult`, naming failure modes.
- `# Safety` on every `pub unsafe fn` in `lean-rs-sys`, naming the precondition. No placeholder patterns ("see lean.h",
  "uphold all Lean invariants").
- Doc links: bare ``[`Type`]`` for crate-root items; `crate::`-qualified for sub-modules.

`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items` must be clean.

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
test -f docs/api-review/lean-rs-worker-public.txt
```
