---
name: test-engineering
description: 'Use for Rust tests: regressions, property tests, integration, compile-fail, benchmarks; minimal repros, law-based tests, brittle/slow/shallow suites, wrong-layer scope.'
---

# Test Engineering

Good tests increase confidence faster than they increase change cost. The goal is not to increase counts. The goal is to
stop real bugs from escaping with the smallest, sharpest set of checks.

Use this skill to decide what to test, where to test it, and what kind of test surface fits the risk. For
performance-sensitive work, route to `optimizing-rust-performance` instead of forcing timing logic into correctness
tests.

## Orient First

Before writing or changing tests, name the pressure:

| Pressure               | What to look for                                                       |
| ---------------------- | ---------------------------------------------------------------------- |
| **False confidence**   | Many tests pass, but a serious bug class could still escape            |
| **Shallow coverage**   | Tests exercise examples, not the contract or invariant                 |
| **Brittle assertions** | Tests fail on harmless refactors, wording, or formatting shifts        |
| **Missing oracle**     | The test has no trustworthy way to tell right from wrong               |
| **Wrong layer**        | The test is too low-level or too end-to-end for the risk               |
| **Regression amnesia** | A real bug was fixed with no minimal regression test                   |
| **Slow suite**         | Test runtime blocks routine use or pushes checks out of the inner loop |
| **Weak generators**    | Property tests generate trivial cases or shrink poorly                 |

If the pressure is unclear, run the audit script first:

```bash
bash .claude/skills/test-engineering/scripts/audit-test-surface.sh <path>
```

## Core Design Principles

These are ordered. If two conflict, the earlier one wins.

### 1. Test contracts and invariants, not implementation steps

A test should answer "what must stay true?" not "what sequence of calls happens today?" If the assertion is about
control flow, local variable names, or exact intermediate representation without that being the contract, the test is
too close to the implementation.

### 2. Test at the highest layer that still isolates the risk

If the bug is a local algebra law, test that local abstraction. If the bug is a user-visible behavior, test the public
surface. Lower-level tests are cheaper to diagnose. Higher-level tests catch integration mistakes. Choose the narrowest
layer that still exercises the real contract.

### 3. Prefer one sharp regression over many vague examples

When fixing a real bug, first add the smallest reproducer that would have caught it. Do not bury it in a giant fixture
or broad integration test unless the bug only exists in that context.

### 4. Prefer executable oracles and algebraic laws for mathematical code

Mathematical and compiler code often has stronger oracles than example outputs: roundtrips, preservation, monotonicity,
idempotence, commutation, or agreement with a simpler reference implementation. Use those when they exist.

For law-shaped testing, open [test-taxonomy.md](references/test-taxonomy.md) and
[mathematical-code.md](references/mathematical-code.md).

### 5. Every "must not hold" rule needs a negative test somewhere

A suite with only positive cases often misses the most dangerous failures. If the system must reject invalid programs,
preserve opacity, prevent unsound duplication, or avoid illegal state transitions, add a test that proves the bad case
stays bad.

### 6. Keep suites fast enough to run routinely

Slow tests create social pressure to skip them. Use the lightest test surface that catches the bug. Keep property-test
sizes and case counts justified. If the real risk is latency, allocation, or asymptotic growth, stop and hand off to
`optimizing-rust-performance`.

### 7. Localize helpers and generators

Helpers should remove noise, not hide the assertion. Keep generators, fixtures, and helpers near the abstraction they
serve unless multiple files genuinely reuse them. Do not build giant `tests/common` utilities that turn tests into
indirect scripts.

## Naming Convention

Use contract-first names. Test names should tell the reader what obligation is protected, not what framework happens to
execute the check.

Treat this as the repo default unless a crate already has a stronger local rule.

### File names

- Use `<area>_<suite>.rs`.
- Allowed suite suffixes are `laws`, `regressions`, `validation`, `generators`, and `helpers`.
- Name files by the contract area, not the mechanism. Prefer `binding_coordinates_laws.rs` over
    `binding_coordinates_proptest.rs`.
- Put property tests and exhaustive tests in `*_laws.rs` when they check laws.
- Keep generator-only code in `*_generators.rs`.
- Keep shared test helpers in `*_helpers.rs`, and only when more than one file genuinely reuses them.

### Function names

- Use snake_case.
- Prefer `<subject>_<property>` for law or behavior tests.
- Use `regression_<bug_or_case>_<expected_behavior>` for regression tests.
- Use `<subject>_<invalid_case>_<rejects_or_fails>` for negative validation tests.
- Avoid `should_*`, `test_*`, and framework-driven names unless a crate already has a compelling established pattern.

### Naming goals

- A maintainer should be able to scan a test file list and know which contracts are covered.
- A failing test name should explain the broken obligation without opening the file.
- Renames are worth doing when the current names reflect test history or tools rather than the actual contract.

## The Audit

Ask these questions in order. Stop at the first "no" and fix that problem.

1. **What bug would escape today?** If you cannot name one, you do not yet know what to test.
1. **What is the real contract?** State the invariant in one sentence.
1. **Is there an oracle?** A law, roundtrip, reference solver, or stable public behavior is stronger than ad hoc
    expected values.
1. **Is the current test at the right layer?** Move up or down if the present surface is too brittle or too indirect.
1. **Is this better as a property than an example?** If many examples only restate the same law, replace them with one
    good property test.
1. **Is the suite over-coupled to representation?** If a harmless refactor would break the test, the assertion is
    probably too shallow.
1. **Is there a missing regression for known bug history?** Fixes without a minimal repro are invitations to regress.
1. **Would a bench be the right guardrail instead?** If the concern is time, allocation, or scaling, switch to
    `optimizing-rust-performance`.

## Working Rules By Context

### New feature

- Add at least one test for the intended contract and one boundary case.
- Prefer the narrowest public or semi-public surface that expresses the feature.
- Do not fill space with smoke tests that only prove compilation.

### Bug fix

- Start with a minimal regression that fails before the fix.
- Assert the specific bad behavior, not just `is_err()` unless the error shape is irrelevant and stable enough.
- If the bug reveals a broader law gap, add one property test too.

### Refactor

- Preserve existing contracts with the smallest credible check.
- If the refactor changes only representation, avoid snapshotting internals.
- Delete or relax tests that were coupled to the old implementation details.

### Mathematical subsystem

- Prefer laws, roundtrips, commutation checks, and reference-model agreement.
- Cover at least one negative case when the subsystem has forbidden behavior.
- Keep generators meaningful and shrinkable. Read [mathematical-code.md](references/mathematical-code.md).

### Public CLI or tooling behavior

- Test visible outcomes: diagnostics shape, files emitted, exit status, command behavior, persisted state.
- Snapshot only stable user-facing structure, not every byte of volatile text.

### Performance-sensitive change

- Do not add benchmark-like loops to `#[test]`.
- Route to `optimizing-rust-performance` for bench or profiling design.
- Keep one correctness regression if the perf bug also had a semantic symptom.

## Failure Smells

| Smell                                                | What it means                               | Fix direction                                              |
| ---------------------------------------------------- | ------------------------------------------- | ---------------------------------------------------------- |
| Many smoke tests, no invariant                       | Activity without confidence                 | Replace with contract-level checks                         |
| `assert!(is_err())` with no error shape              | The rejection contract is underspecified    | Check the relevant error variant or stable diagnostic fact |
| Full-string snapshots for unstable diagnostics       | Test is coupled to wording churn            | Assert stable fragments or structured fields               |
| Large fixtures instead of minimal repros             | Regression is hard to diagnose              | Shrink to the smallest failing case                        |
| Helper layers hide the actual assertion              | Test logic is opaque                        | Inline the key assertion and narrow helpers                |
| Property tests with weak generators                  | Randomness without coverage                 | Improve the generator and shrink story                     |
| Integration tests for a local invariant              | Test is too slow and noisy                  | Move the check down a layer                                |
| Slow proptests with unjustified case counts          | The suite will be skipped                   | Reduce sizes/cases and justify the remainder               |
| Benchmark-like logic inside `#[test]`                | Performance risk is using the wrong surface | Hand off to `optimizing-rust-performance`                  |
| Duplicate tests across layers for the same bug class | Maintenance cost without new confidence     | Keep the most diagnostic layer and delete the rest         |

For more examples of shallow or brittle tests, read [failure-smells.md](references/failure-smells.md).

## Validation

Run the narrowest credible verification first. In this repo, prefer:

```bash
cargo nextest run -p <affected-crate>
```

Use `make test-kernel` or `make test-frontend` only when the change spans multiple crates or layers.

When the correct answer is a bench or profiling guardrail rather than a correctness test, stop and switch to
`optimizing-rust-performance`. The handoff rules are in [perf-handoff.md](references/perf-handoff.md).

## Related Skills

- `binding-structure-audit` for binders, substitution, weakening, or locally nameless laws
- `partial-cbpv-kernel` for split-kernel changes with theory-sensitive tests
- `effect-inference-theory` for effect inference obligations and boundary cases
- `control-flow-linearity` for handler multiplicity and negative linearity tests
- `homotopy-type-theory-cubical-models-univalence-and-w-types` for cubical laws and boundary behavior
- `kernel-boundary-enforcement` when non-kernel tests must use wrappers, not internal terms
- `theory-implementation-alignment` when the job is to compare spec to code before writing tests
- `optimizing-rust-performance` when the missing guardrail is a bench or profile, not a correctness test
