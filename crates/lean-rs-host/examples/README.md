# Host examples

Build these from the workspace with `-p lean-rs-host`. They exercise same-process host services against `fixtures/lean`;
long-running import-heavy applications should use the [production worker pattern](../../../docs/production-hosting.md).

## Migrating the six embedding examples

These former executables now build as Rust libraries. Compile any of them with
`cargo build -p lean-rs-host --example NAME`. Call the entrypoint from the application that admits the session or
imports and owns the request.

| Entrypoint | Supply from the application |
| --- | --- |
| `theorem_query::query` | A mutable admitted session. |
| `proof_check::check` | The session and its attested kernel request. The example revalidates returned evidence. |
| `meta_query::infer` | The session and its attested term. |
| `progress::report_and_cancel` | A session importing `LeanRsFixture.Handles`. |
| `process_module_query::run` | The session and an attested module-source request for the selector returned by `parse_query`. |
| `import_residue_probe::measure` | The host capabilities, their ordered admitted import profiles and the session-drop choice. `parse_profiles` reads module names. |

## Setup for runnable fixture examples

```sh
(cd fixtures/lean && lake build)
```

See the [shipping recipe](../../../docs/recipes/ship-crate-with-lean.md) for build requirements. Host Meta and
elaboration services come from the bundled host shims, while the fixture supplies test declarations and expressions.

The examples derive fixture paths from compile-time `env!("CARGO_MANIFEST_DIR")`. Setting that variable at runtime does
not change the compiled path. Missing artifacts fail at the build, library-load, or capability-load step that needs
them; see the reported diagnostic code and [diagnostics guide](../../../docs/diagnostics.md).

## Choose an example

| Command | Behavior and expected output |
| --- | --- |
| `cargo run -p lean-rs-host --example tour` | Composes host open, capability load, import, elaboration, kernel checking, a bulk query, and `Meta.whnf`; prints `name=<stage> elapsed_us=<u64>`. |
| `cargo run -p lean-rs-host --example lake_build_helper` | Runs a Lake shared-library build through `lean-toolchain`; prints Cargo rerun directives and the resolved `dylib=...` path (`.dylib` on macOS, `.so` on Linux). |
| `cargo run --release -p lean-rs-host --example long_session_memory` | Emits workload parameters, import counters, and RSS checkpoints across imports, warm queries, and drops. |

The long-session executable installs a tracing subscriber. Embedding applications configure their own subscriber; set `RUST_LOG=lean_rs=debug` for their structured
spans. Query/check examples propagate errors and print their diagnostic code; the tour uses direct assertions for its
walkthrough. Do not infer one error-handling policy from all examples.

If a Meta response is `Unsupported`, inspect the installed host-shim capability and rebuild those shims as needed. The
fixture does not export the host Meta contract. `TimeoutOrHeartbeat` reports the bounded Meta request's limit; inspect
its `LeanMetaOptions` before changing a budget.

The memory reproducer is bounded by repository defaults. Increase `LEAN_RS_LONG_SESSION_IMPORTS` only after inspecting
the previous peak and setting an explicit `LEAN_RS_LONG_SESSION_MAX_RSS_KIB`. RSS checks act before later imports, so
one import can overshoot a ceiling. It is a retention diagnostic, not an unbounded production soak test. Reproduction
modes and historical measurements live in [Long-session memory](../../../docs/safety/long-session-memory.md); scripted
captures live in the [profiling guide](../../../profiling/README.md).

## Callback examples

Callbacks belong to `lean-rs`, below host sessions:

```sh
cargo run -p lean-rs --example interop_callback
cargo run -p lean-rs --example string_streaming
```

Follow [Downstream interop](../../../docs/recipes/downstream-interop.md) for export signatures, shim packaging, callback
lifetimes, and string payloads. Read the [concurrency contract](../../../docs/architecture/concurrency.md) before
moving integration work to another OS thread.

## Embedding the query and import probes

The former standalone module-query and import-residue commands accepted a file or a list of module names. Their replacement entry points take authority from the application that admits the native images and imports. Compile both examples with:

```sh
cargo check -p lean-rs-host --example process_module_query --example import_residue_probe
```

Copy the example module into the embedding binary. For a module query, call `parse_query(kind, arguments)` to obtain the selector. Have the application's source policy check the exact file contents, selector, options and session environment, then pass the resulting typed request to `process_module_query::run(&mut session, &request)`. It prints the same bounded query outcomes.

For residue measurements, call `parse_profiles(&text)`. Have the application's import policy admit each ordered module list against the same capabilities and retain those receipts in order. Call `import_residue_probe::measure(&caps, &profiles, drop_each)` once in a fresh process. Set `drop_each` to false to retain every session through the final memory sample. These two examples require an embedding application with an admission policy; building the example alone produces no runnable CLI.

### Controlled fixture runs

The `tour` and `long_session_memory` executables use the host test fixture admission helper. Cargo can check their code without a runtime policy. Running them requires a separately qualified build and launcher. In particular, leave both `LEAN_RS_IMPORT_PROFILE_TRACE_PROFILER_OUTPUT_DIR` and `LEAN_RS_IMPORT_PROFILE_TRACE_PROFILER_OUTPUT` unset. Admission rejects every nonempty file destination, including absolute paths.

For each executable, the fixture owner reviews the exact source closure, binary and load commands, native artifacts, transitive release inputs, import graphs and source operations. The linker output must be placed in the held `bin` directory. After linking, the owner places `host-services-linked-review.json` and the authenticated `release-inventory.json` in the held `inspection` directory. The post-link record specifies the canonical executable path, its SHA-256, the reviewed source identity, and the release-inventory and graph-policy SHA-256 values. Build-time settings embed the source identity, release-inventory digest, exact graph policy, artifact record and private import inventory. Review includes every selected elaborator, extension, helper and transitive native image. The launcher verifies the actual executable, link closure and stable release prefix **before exec**, sanitizes the environment and keeps custody through observed exit.

Host-service tests exercising fresh Lake projects compare their build with a separately prepared, inventoried copy under `fixture/host-service-projects/<case>` before importing it. Missing-module cases require the named module to be absent from the private and release roots. The optional-service negative needs a separately reviewed shim omitting the WHNF export. Each such case needs its own applicable policy and process. The current scalar and scoped-theorem qualifications cover only their recorded inputs.
