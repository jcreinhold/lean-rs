# Architecture charter

`lean-rs` is a Rust runtime bridge to Lean 4. Lean owns elaboration, kernel checking, proof objects, universes, `MetaM`,
and dependent-type meaning. Rust owns linking, runtime initialization, ABI conversion, module loading, error boundaries,
scheduling, diagnostics, batching, process isolation, and packaging.

An API belongs here when it hides a hosting decision while preserving Lean's capabilities. Rust must not recompute Lean
semantic facts, and Lean services must not depend on a Rust application's scheduler or packaging policy.

## Crate boundaries

| Crate | Responsibility |
| --- | --- |
| `lean-rs-abi` | Static supported-toolchain, required-symbol, and `lean.h` layout metadata. No build script, extern declarations, link directives, or installed-toolchain probe. |
| `lean-toolchain` | Toolchain discovery, live build-time identity, fingerprints, Lake builds, artifact manifests, linking diagnostics, and native command ownership helpers. Builds without Lean when no runtime is required. |
| `lean-rs-sys` | Raw Lean C ABI declarations and inline mirrors. Public FFI types are opaque; layout representations remain crate-private. Its build script checks the active header digest and emits runtime link directives. |
| `lean-rs` | Runtime and thread handles, object ownership, ABI conversion, library/module loading, checked typed exports, callbacks, and errors. |
| `lean-rs-host` | Standard services: imports, sessions, elaboration, evidence, declaration/module queries, and bounded `MetaM`. Owns its Lean host-shim packages. |
| `lean-rs-worker-protocol` | Framed parent/child messages and owned wire data; no Lean runtime link. |
| `lean-rs-worker-parent` | Child supervision, deadlines, cycling, worker sessions, and diagnostics; no Lean runtime link. |
| `lean-rs-worker-child` | Lean-linked child runtime and worker executable. |

The profiling harness is an unpublished workspace member; the ABI fuzz harness is a separate unpublished package. Public
crates share a version, with curated surfaces recorded in
[API baselines](https://github.com/jcreinhold/lean-rs/tree/main/docs/api-review). The static ABI crate underlies both
raw bindings and toolchain helpers; `lean-toolchain` does not depend on `lean-rs-sys`. The safe runtime uses both,
`lean-rs-host` builds on it, and the worker boundary wraps host services.

Use the highest layer that fits the job:

- `lean-rs` for custom same-process exports and callbacks;
- `lean-rs-host` for common checked Lean services;
- worker crates for fatal-exit containment, request watchdogs, or process memory reset.

Downstream applications own their exports and what they mean. The worker API is a supervised process service, not a
remote mirror of every `LeanSession` operation. Its model is specified in [Worker runtime semantics](worker-runtime.md).

## Ownership and lifetime

`Obj<'lean>` owns a Lean reference count and carries a lifetime tied to a `LeanRuntime` borrow. Semantic handles and
host/session types propagate that lifetime. `LeanRuntime::init` initializes the process once and returns a static
anchor; Lean-derived handles remain thread-confined. [Concurrency](concurrency.md) describes initialization and thread
attachment, and the [safety model](safety.md) defines the unsafe obligations.

Dropping an object or session releases its owned references. It does not reset Lean's process-global imports,
initializers, extension registries, or allocator state. Full-session workloads reuse environments and bound fresh
imports; process exit is the reset boundary. See [Production hosting](../production-hosting.md) and the
[retention measurements](../safety/long-session-memory.md).

## Knowledge hidden by each layer

| Boundary | Hidden decisions |
| --- | --- |
| Safe runtime | Initialization order; scalar/heap tags; constructor layout; owned versus borrowed reference counts; object conversion; module initializer symbols; dynamic loader calls; callback context pointers and panic containment. |
| Host services | Host-shim symbol binding and ABI; Lean monad/state plumbing; environment handles; batching; cache bookkeeping. |
| Worker services | Child handles; request framing; generation and cleanup ownership; request interruption; bounded event buffers; replacement and admission policy. |
| Build tooling | Toolchain discovery order; the Lake questions asked and how their answers are read; artifact manifests; native command lifetimes. |

Public callers choose capabilities, imports, budgets, commands, and typed results. They should not need to reconstruct
the hidden decisions to use an operation safely. Explicitly unsafe raw FFI remains available through `lean-rs-sys` for
applications that accept those obligations.

Checked export lookup relies on trusted signature metadata from the capability's author. It checks ABI compatibility; it
does not prove that native code implements the declared semantics. Arbitrary symbol lookup stays explicitly unsafe. The
charter's semantic boundary and Rust memory-safety boundary are separate obligations.

## Preserved capabilities

Applications can load compiled Lean modules, invoke typed exports, ask the elaborator and kernel questions through
bounded host services, batch requests, reuse sessions, and receive diagnostics or rows across a process boundary. The
bridge keeps Lean's semantic authority intact while presenting owned Rust results or lifetime-bound Lean handles.

Lower layers remain usable APIs for custom integrations. Common host users should not have to assemble raw runtime, ABI,
or shim dispatch themselves.

## Rejected designs

- **Wrap all of `lean.h` as a nominally safe facade.** This exposes ownership, initialization, and layout obligations to
  callers. Add safe operations that discharge those obligations; keep raw access in `lean-rs-sys`.
- **Reimplement Lean semantics in Rust.** Independent `Expr`, universe, environment, or proof interpretations would
  become a second authority. Rust semantic handles wrap Lean values; Lean computes their meaning.
- **Split every internal module into a public crate.** A crate needs a real independent consumer and compatibility
  boundary. ABI metadata and build tooling have link-free consumers; runtime internals remain modules of `lean-rs`.
- **Bundle typed FFI and standard host services into one required interface.** Calling a user export must not require
  the theorem-prover host-shim contract. Generic callback helpers live below host services.
- **Adopt an external raw binding without the required ABI contract.** Raw bindings must support the project's opaque
  types, checked header window, symbol allowlist, inline mirrors, and packaging. A wrapper around an incompatible
  binding does not establish those guarantees.
- **Treat in-process drop as runtime recycling.** Lean provides no global finalize/reinitialize contract that can
  invalidate every old object, symbol, task, and initializer safely. Use process cycling.

Performance claims need a named workload, command, toolchain, and measured result. Preserve the limits of each
measurement; follow [Performance](../performance.md) when comparing changes.
