# lean-rs

lean-rs runs Lean 4 from Rust. Lean owns what terms mean: elaboration, kernel checks, proof objects, universes and
`MetaM`. This workspace owns linking, runtime start-up, object conversion, loading, error boundaries, worker processes,
diagnostics and packaging.

Read [docs/architecture.md](docs/architecture.md) before you change code. It says who owns what and names the tests
behind each invariant; change an invariant, its tests and its entry together. The `release-lean-rs` and `bump-toolchain`
skills in `.claude/skills/` hold the release and toolchain procedures.

## Boundaries

- Raw `lean_*` declarations belong only in `lean-rs-sys`. Add a symbol to the matching
  `crates/lean-rs-sys/src/<category>.rs`, to `REQUIRED_SYMBOLS` in `crates/lean-rs-abi/src/symbols.rs`, and to the
  probes in `crates/lean-rs-sys/tests/linkage.rs`.
- `lean-rs-abi` is static metadata that links nothing: no build script, `extern` declarations, linker directives or
  probes of an installed toolchain. `lean-toolchain` finds the live toolchain and reports its identity.
- `lean-rs` owns safe runtime handles, conversions, loading and callbacks; `lean-rs-host` adds the standard Lean
  services. The worker protocol and parent crates do not link `libleanshared`; the worker child does.
- Public FFI types stay opaque. Never expose `LeanObjectRepr` outside `lean-rs-sys` or give an FFI type public fields.
  Safe layers do not re-export raw symbols; a caller that needs raw FFI depends on `lean-rs-sys` itself.
- Keep the `lean_rs::*` re-exports curated. Add a trait when a second concrete type needs it.
- Add a public item only when a downstream caller uses it. Write no guide for other embedders, and name no downstream
  project in code or docs.

## Checks

Run these before each commit:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo xtest
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
uv run --no-project python scripts/check_rust_line_width.py HEAD
```

- `cargo xtest` runs `cargo nextest run --workspace`; `cargo xtest-ci` adds the `ci` profile, which retries a failed
  test once. Install nextest with `cargo install cargo-nextest --locked`. Nextest gives each test its own process.
  Lean's process-wide tables (interned names, its heap, registered environments) only grow, and dropping a session does
  not shrink them, so one long `cargo test` process runs out of memory. Doctests do not run under nextest.
- The line-width check compares added and edited Rust lines with `max_width` in `rustfmt.toml`, including strings and
  macro calls that rustfmt leaves long. It sees only files Git knows: stage new files or mark them with `git add -N`.
  Pass a pull request's base commit to check its whole diff.
- When you change private docs, also run
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items`.
- `scripts/prerelease.sh` runs the CI gates on the newest supported Lean release: it builds the Lake fixtures, runs the
  worker tests below, and diffs the public API. Run it before a release and after you change fixtures, shims, templates
  or the worker crates. Commit changes under `fixtures/` first: the qualifier copies only tracked files and refuses a
  fixture source that differs from `HEAD`. Run no other build or clean in the checkout while it runs. `--help` lists its
  options; [`.github/workflows/ci.yml`](.github/workflows/ci.yml) holds the full CI list.
- For callback or progress changes, also run the `lean-rs` examples `interop_callback` and `string_streaming`. Like the
  tests, they load the fixtures that `scripts/prerelease.sh` builds and `scripts/select_test_fixtures.py` selects.
- For a performance change, compare with a saved Criterion baseline on the same toolchain and machine, for example
  `cargo bench -p lean-rs-host --bench session -- <filter> --baseline <name>`, and put the command, revision, workload
  and results in the commit message.

### Memory

- `.config/nextest.toml` runs two test processes; memory tests and tests that build or load the shipped template take
  every slot. `.cargo/config.toml` sets `build.jobs = 1`, `RUST_TEST_THREADS=1`, `LEAN_RS_NUM_THREADS=1`, and
  `LEAN_RS_LEAN_MAX_MEMORY_KIB=1572864` as each worker child's memory cap. Set `NEXTEST_TEST_THREADS=1` on a small
  machine or container; raise it, or `CARGO_BUILD_JOBS`, when memory allows.
- A variable set in the shell overrides Cargo's `[env]` table, so `LEAN_RS_NUM_THREADS=8 cargo bench -p lean-rs` gives a
  benchmark more Lean threads. An empty `LEAN_RS_NUM_THREADS=` lets Lean choose, with a warning; unsetting the variable
  restores Cargo's `1`.
- A host session imports the Lean compiler environment, and two fresh imports in one process can take several GiB. So a
  host session refuses to open inside a `cargo test` binary. Run host-session tests under nextest, for example
  `cargo nextest run -p lean-rs-host --test session_leak_loop`. To debug one such test under `cargo test`, set
  `LEAN_RS_ALLOW_CARGO_TEST_HOST_IMPORTS=1` for that test alone and watch its memory.
- Ignored tests run only by name: `cargo nextest run -p <crate> --test <binary> --run-ignored only -E 'test(=<name>)'`.
  The leak loop in `session_leak_loop` also needs `LEAN_RS_RUN_IMPORT_HEAVY_TESTS=1`, so that `--run-ignored all` cannot
  start it by accident; bound it with `LEAN_RS_LEAK_LOOP_ITERS`.
- A worker child that imports the compiler environment several times can outgrow a 7 GB CI runner. Give each new worker
  test the lightest imports that show what it checks: the `Init`-only fixture modules unless it elaborates.

### Worker tests

The worker child's integration tests load host shims that `scripts/qualify_metadata.py` builds and checks. It builds one
shim package from tracked Lake inputs under `target/metadata-qualification/shim-package`, with the installed Elan
toolchain, and hands it to the tests. On the newest supported release, `scripts/prerelease.sh` and CI run:

```sh
uv run --no-project python scripts/qualify_metadata.py broken-import --emit-host-selection --run-typed --workspace-shims
uv run --no-project python scripts/qualify_metadata.py workspace-tests "$LEAN_RS_TEST_REMAINING_FILTER"
```

The first command runs the broken-import control and the typed tests and prints `LEAN_RS_TEST_ORDINARY_HOST_SHIMS` and
`LEAN_RS_TEST_REMAINING_FILTER`. With `LEAN_RS_TEST_ORDINARY_HOST_SHIMS` exported, `workspace-tests` runs the rest of
the workspace under nextest on the same shims.

For a metadata, doctor or host-handle change, run the control, then the affected interop cases:

```sh
uv run --no-project python scripts/qualify_metadata.py broken-import --emit-host-selection
uv run --no-project python scripts/qualify_metadata.py interop "$SHIM_MANIFEST" \
  --case runtime_and_capability_metadata_are_separate \
  --case session_import_profile_reaches_child_import
```

Set `SHIM_MANIFEST` to the `shim_manifest_path` in the `LEAN_RS_PREPARED_HOST_ARTIFACTS` line that the first command
prints. `interop --help` lists the cases. A failed run keeps its state, and later runs refuse to start until it is gone.
Check that no worker still runs (`pgrep -fl 'metadata-worker|lean-rs-worker-child|qualify_metadata'`), then remove
`broken-import` and `shim-package` under `target/metadata-qualification/` and
`fixtures/metadata-qualification/app/target/scalar-entry`; leave `.ordinary.lock`.

### Linux in Docker

`scripts/test_linux_docker.sh` runs tests that need no Lean in a throwaway Linux container on Docker Desktop. By default
it runs the unit tests of `lean-rs-worker-parent`, which start, fail and reap fake children, on `linux/amd64`:

```sh
scripts/test_linux_docker.sh
scripts/test_linux_docker.sh --platform linux/arm64   # when x86-64 emulation is missing or slow
scripts/test_linux_docker.sh --package lean-rs-abi --lib
scripts/test_linux_docker.sh --filter startup_failure
```

`--package` takes only `lean-rs-abi`, `lean-rs-worker-protocol` or `lean-rs-worker-parent`; `--test NAME`, `--lib` and
`--filter TEXT` choose the libtest run. It needs Docker Desktop, GNU `timeout` or `gtimeout`, and 30 GiB free on the
checkout's disk. Check the reported test count, since a mistyped filter selects none. It loads no Lean, so it replaces
neither nextest nor the Linux CI jobs.

## Public API

`docs/api-review/<crate>-public.txt` holds the `cargo public-api` output for seven crates; `lean-rs-abi` has none. CI
fails when a crate's API differs from its file. An intended change updates the file in the same commit; never regenerate
one to hide a change you did not intend. Use the pinned pair below, which `ci.yml`, `release.yml` and
`scripts/prerelease.sh` also use; when a pin moves, change all three and regenerate every file.

```sh
cargo install cargo-public-api@0.52.0 --locked
rustup toolchain install nightly-2026-06-18 --profile minimal
mkdir -p target/tmp
for crate in lean-rs-sys lean-toolchain lean-rs lean-rs-host \
             lean-rs-worker-protocol lean-rs-worker-parent lean-rs-worker-child; do
  cargo +nightly-2026-06-18 public-api --simplified -p "$crate" > target/tmp/public.txt &&
    mv target/tmp/public.txt "docs/api-review/$crate-public.txt" || break
done
```

Before you add public API, check that it owns one clear job, hides ABI and lifetime details, and makes callers pay only
for the work they ask for. Avoid pass-through wrappers, traits with one implementor, broad crate-root re-exports, and
error variants for internal stages of one failure. Each fallible public function documents its errors, and each public
unsafe function states concrete preconditions; "see lean.h" is not one.

## Unsafe code

- The workspace denies `unsafe_code`; only `lean-rs-sys` opts out for the whole crate. Elsewhere, allow unsafe code on
  the smallest scope and say why in the commit message.
- Each `unsafe` block has a `// SAFETY:` comment that names its invariant; each public unsafe function has a `# Safety`
  section.
- Test new or reshaped unsafe code against the ways it could fail: focused tests, reference-count stress, Miri,
  sanitizers or fuzzing, as fits.
- A reviewer other than the author signs off on new unsafe code. Do not merge your own.
- Safe callers never take on Lean reference counts or pointer validity.

## Compatibility and releases

- `SUPPORTED_TOOLCHAINS` in `crates/lean-rs-abi/src/supported.rs` lists the supported Lean releases; the
  `bump-toolchain` skill changes it. Decide how to stay compatible before you change Lean object layout, ownership or
  the initializer protocol, and update docs/architecture.md to match.
- Do not push or tag a change that spans repositories until the agreed checks pass on matched local sources, and do not
  publish an SDK version only to test a downstream crate. The `release-lean-rs` skill tests matched sources and ships
  one batch in dependency order.

## Storage

- Keep no local evidence or work output by default. If a release or maintainer decision needs a lasting record, commit
  one short note to Git; keep no local archive beside it.
- Never store repository work or evidence in `~/Documents/Codex`, `/private/tmp` or `/tmp`. Put nothing but Git's data
  under `.git`, and do not use another path, an archive, compression, an alias or split files to get around a storage
  limit.
- Use the installed Elan toolchain and one Cargo target directory. Do not copy a Lean release, a toolchain or a target
  directory to keep or reuse.
- Before a command that can write a lot of data, state its output path, its largest size, when it expires and how you
  will remove it, and start only if that fits. Remove transient output when the command ends or the work stops.

## Completion

- Non-test `unwrap()`, `expect()` and `panic!` need a comment that proves they cannot fail. Leave no `TODO`, `todo!()`
  or `unimplemented!()`.
- Fix a cause in the module that owns it. When the design changes, update docs/architecture.md in the same commit, and
  fix stale guidance here.
- Keep each commit to one logical change, and say why it is needed and what checked it. Do not amend a shared commit.
- Report a bug with the Lean version, OS, Rust version and the exact failing command.
