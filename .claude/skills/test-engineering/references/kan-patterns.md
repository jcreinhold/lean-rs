# Kan Patterns

## Preferred verification commands

Use `cargo nextest run`, not `cargo test`.

Common commands:

```bash
cargo nextest run -p <crate>
make test-kernel
make test-frontend
make rust-test
```

## Common test locations

- integration tests: `tests/` or `tests/it/`
- unit tests: inline `mod tests`
- benches: `benches/`

## Common Kan test shapes

- kernel math: laws, under-binder coverage, negative theory boundaries
- registry/storage: roundtrip, ordering, identity, conflict detection
- pipeline passes: preservation and semantic equivalence
- CLI/tooling: visible behavior and persisted state

## Prefer nearby authorities

Before writing tests, inspect:

- nearby docs/spec or architecture notes
- nearby issue or regression context
- existing tests in the same crate
- existing benches before inventing a new perf surface
