# Loader And Artifact Boundary

The v0.1.1 release validated the worker foundation, but it also exposed a
separate source of fragility: shipping Lean shared libraries is still too easy
to do correctly only by accident. A caller should not have to know which dylib
must be opened first, which one needs global symbols, how long a transitive
dependency handle must remain alive, which environment variables the platform
loader consults, or why docs.rs builds without Lean installed.

Those details are real, and they are volatile. They belong behind the
`lean-rs` crate boundaries, not in every downstream build script, test helper,
or worker launcher.

## Chosen Boundary

The shipping stack has three responsibilities:

```text
CargoLeanCapability artifact description
    -> LeanCapability bundle loader
    -> LeanWorkerCapabilityBuilder / LeanWorkerPool
```

- `lean-toolchain` owns the build artifact description. It builds the Lake
  shared-library target, resolves Lake's dylib naming, emits Cargo rerun and
  link directives, and records enough artifact metadata for runtime code to
  reopen the same capability without rediscovering Lake output conventions.
  Release gates also simulate docs.rs from normalized crate tarballs with
  Lean, Lake, and Elan hidden from `PATH`, so package drift fails before
  crates.io receives immutable uploads.
- `lean-rs` owns runtime loader lifetime and symbol visibility. It opens the
  capability and any dependent Lean dylibs in the required order, keeps those
  handles alive for the full capability lifetime, preflights the manifest and
  artifacts into stable diagnostics, initializes the requested module, and
  hides platform loader differences.
- `lean-rs-worker` owns the process boundary. It locates and starts the
  app-owned worker child, builds the child environment, opens the capability in
  the child, reports bootstrap diagnostics, and keeps protocol pipes private.

Callers still know their own Lake package and root module, exported command
names, typed request and row schemas, and whether the workload should run in
process or behind worker isolation. Those are application decisions. Loader
order, `RTLD_GLOBAL`, `LD_LIBRARY_PATH`, `DYLD_*`, Lake `.lake/build/lib`
layout, docs.rs no-Lean behavior, and worker protocol pipes are not.

## Rejected Designs

**Caller-managed loader order.** Rejected. It would make every user understand
which Lean dylibs are dependencies, which ones must be opened globally, and how
long each handle must stay alive. That repeats the same fragile dynamic-loader
contract at every call site and test fixture.

**Scattered helper fixes.** Rejected. Test-only handle leaks, local
`LD_LIBRARY_PATH` workarounds, special docs.rs branches in individual build
scripts, and one-off worker child path probes can fix one failure at a time,
but they keep the volatile knowledge duplicated. The next platform or package
layout change would create another incident.

**Deep loader/artifact boundary.** Chosen. The build helper records the
artifact; the runtime loader opens the complete bundle and anchors it; the
worker builder transports that bundle into an isolated child process. Each
layer has a different abstraction and hides a distinct kind of complexity.

## Brittleness Classes

### Lean Process-Global State

Lean runtime initialization, imported modules, interned names, persistent
objects, allocator state, and module initializers are process-scoped. Dropping a
Rust wrapper does not necessarily undo those effects. `lean-rs-host` exposes
trusted in-process work; `lean-rs-worker` remains the production boundary for
panic containment and memory reset.

### Dynamic-Loader Symbol Visibility

Lean-generated shared libraries can reference symbols emitted by imported Lean
libraries. On ELF platforms, later libraries may need earlier libraries to have
been opened with globally visible symbols. On macOS the loader can appear more
forgiving, which makes Linux-only failures easy to miss. Normal callers should
not choose global visibility manually.

### Transitive Dylib Lifetime

Opening a dependency globally is not enough if the Rust handle is dropped while
another Lean dylib still relies on its symbols. The loader boundary must anchor
dependent handles for as long as the capability can execute code that depends
on them. Test-only leaks are a symptom that lifetime belongs in the loader
abstraction.

### docs.rs Builds Without Lean

docs.rs builds crate documentation in an environment where Lean, Lake, and Elan
may be absent. Published crates must make documentation builds independent of a
local Lean installation. A docs.rs guard is not a substitute for checking
package contents and build behavior before publishing.

### crates.io Package Drift

Published tarballs are immutable. A missing template, shim, README, benchmark,
`lean-toolchain`, `lakefile.lean`, or generated doc baseline cannot be repaired
in-place after release. The package boundary must include explicit contents
checks so local builds, docs.rs builds, and downstream builds see the files the
docs describe.

### Worker Child Bootstrap Environment

Worker applications ship an app-owned worker child. The parent must find that
binary, pass capability artifact paths, preserve only the environment needed by
Lean and the worker protocol, and report startup failures with actionable
diagnostics. Callers should not debug child pipes, inherited environment drift,
or dependency-binary lookup rules.

## Crate Responsibilities

### `lean-toolchain`

`lean-toolchain` hides:

- Lean and Lake discovery;
- `lake build <target>:shared`;
- Lake shared-library naming across supported toolchains;
- Cargo rerun directives and link directives;
- package include expectations for Lean sources and Lake metadata;
- artifact metadata such as package, module, dylib path, search roots,
  toolchain fingerprint, and dependency dylibs known at build time.

The lower-level `build_lake_target` and raw link-directive helpers remain for
advanced build systems. They are not the canonical shipped-crate path.

### `lean-rs`

`lean-rs` hides:

- `LeanLibrary` handle lifetime;
- dependency open order;
- global symbol visibility;
- manifest preflight and repair hints;
- module initializer symbol names and sequencing;
- platform loader differences;
- nullary Lean global-vs-function export classification.

`LeanCapability` is the normal same-process runtime surface for shipped
capabilities. `LeanCapabilityPreflight` is the doctor-style surface for
checking a manifest-backed capability before opening it. `LeanLibrary::open`
and `LeanLibrary::open_globally` remain public for advanced L1 interop and
focused tests, but they are escape hatches: using them means the caller has
chosen to manage loader details explicitly.

### `lean-rs-worker`

`lean-rs-worker` hides:

- app-owned worker child lookup;
- child environment construction;
- capability startup inside the child;
- bootstrap and capability diagnostics;
- request timeouts, restart policy, and process isolation;
- worker protocol frames, pipes, and child lifecycle details.

The worker builder and pool should consume the same artifact description as the
same-process loader. Worker parent-facing examples must not ask callers to open
Lean dylibs, set loader paths, or pass callback handles across the process
boundary.

## Normal And Advanced Paths

Use the highest-level surface that matches the job:

- Use `CargoLeanCapability` in `build.rs` for crates that ship Lean source.
- Use `LeanCapability` to call a built capability in process.
- Use `LeanWorkerCapabilityBuilder` or `LeanWorkerPool` when the application
  needs process isolation, request timeouts, live rows, memory cycling, or
  crash containment. Their normal packaged-app input is the same
  manifest-backed capability descriptor that `LeanCapability` consumes.
- Use `LeanLibrary::open`, `LeanLibrary::open_globally`, raw link directives,
  and low-level worker APIs only for advanced interop, diagnostics, and tests.

## Regression Gates

`crates/lean-rs-worker/tests/loader_regressions.rs` protects the public
packaged-app path. It builds the shipped-crate template, then runs the
same-process binary and worker example with `LD_LIBRARY_PATH`, `LD_PRELOAD`,
`DYLD_LIBRARY_PATH`, `DYLD_FALLBACK_LIBRARY_PATH`, and `DYLD_INSERT_LIBRARIES`
removed. The test proves the canonical path relies on build artifacts, rpath,
and the bundle loader rather than a developer shell's loader environment. The
same file also checks the template package list and proves a public
`LeanCapability` bundle keeps a transitive Lean dependency alive after the
opener helper returns.

Worker bootstrap is checked through
`LeanWorkerCapabilityBuilder::check()`. The report validates app-owned child
resolution, executable status, manifest-backed capability preflight, protocol
handshake, import session startup, and optional metadata expectations before a
real command runs. The worker regression still keeps `PATH` because current
host-session import startup uses Lean tooling; the bootstrap check now reports
that deployment boundary explicitly instead of asking callers to manage child
pipes or dynamic-loader variables.

This is not a narrow fix for one Linux or docs.rs failure. The failures point
at a common design problem: volatile loader, package, and bootstrap decisions
were visible in too many places. The next hardening prompts move those
decisions behind the crates that can own and test them.
