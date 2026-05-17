# Failure Smells

## Example-heavy, contract-light

If many tests differ only by fixture values, you may be enumerating a law badly. Replace them with one property test if
the oracle is uniform.

## Assertion too weak

`assert!(result.is_err())` is often not enough. If the contract is a specific rejection reason, check the variant or a
stable diagnostic fact.

## Assertion too brittle

Full snapshots of volatile diagnostics, formatting, or internal IR make harmless refactors painful. Keep only the stable
part of the contract.

## Helpers that hide the test

If a helper turns three lines of setup into one line of mystery, it is too much. Helpers should remove repetition, not
obscure the invariant.

## Duplicate coverage at multiple layers

If a local invariant is tested in unit, integration, and end-to-end form with no different bug class caught at each
layer, delete the most expensive duplication.

## Performance disguised as correctness

Loops, giant random cases, or timing assumptions inside `#[test]` usually mean the wrong surface was chosen. Move that
concern into a bench or profile.
