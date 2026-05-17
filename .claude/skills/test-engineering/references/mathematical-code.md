# Mathematical Code

Mathematical code often has stronger tests than examples.

## Prefer law-shaped checks

Reach for:

- roundtrip: parse/print, store/load, open/close, eval/quote
- preservation: type, shape, or metadata survives a transformation
- agreement: optimized path matches a simpler reference implementation
- algebraic laws: associativity, commutativity, idempotence, monotonicity
- opacity: forbidden reductions or observations remain forbidden

## Oracles

Best to worst:

1. simple reference implementation
1. algebraic law
1. stable public behavior
1. curated example outputs

If you cannot explain the oracle, you are not ready to write the test.

## Generator guidance

- Generate owned templates, then build borrowed or arena-backed values from them.
- Bias toward boundary cases: zero, one, empty, singleton, deeply nested, repeated names.
- Make shrinking preserve meaning. A shrinking strategy that destroys the invariant under test is noise.

## Negative tests

Every "must not hold" theorem deserves a test somewhere.

Examples:

- invalid effect use must be rejected
- unsound duplication must stay rejected
- neutral forms must remain opaque
- non-convertible universes must stay non-convertible

Do not leave negative obligations implicit.
