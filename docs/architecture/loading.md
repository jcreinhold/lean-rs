# Loader and Artifact Boundary

Shipped capabilities use one build-time artifact description for same-process loading and worker startup:

```text
CargoLeanCapability -> manifest-backed LeanBuiltCapability
  -> LeanCapability, or LeanWorkerSessionConfig::manifest_backed in a worker
```

Applications supply their Lake package/module, exports, and isolation policy. The library asks Lake where the artifacts
are and owns dependency load order, symbol visibility, handle lifetime, module initialization, preflight, and worker
bootstrap. Repeating those details in each consumer's build script or loader would produce platform-specific failures.

## Build artifacts

`lean-toolchain::CargoLeanCapability` builds the shared Lake target, emits Cargo rerun/link directives, and writes a
manifest containing the primary dylib, declared dependency dylibs, initializer metadata, toolchain fingerprint, and
trusted export signatures. `LeanBuiltCapability` is the link-free descriptor shared by the runtime and worker parent.

Declare separately built dependency dylibs through the builder so both loading paths see the same order and visibility
requirements. `build_lake_target` and raw link directives remain available for custom build systems; the
[shipped-crate template](https://github.com/jcreinhold/lean-rs/tree/main/templates/shipped-lean-crate) demonstrates the
normal path. Shared build coordination belongs to the [build-and-link contract](build-and-link.md).

## Runtime loading

`LeanCapability::from_build_manifest` preflights the artifact and opens its `LeanLibraryBundle`. The bundle retains the
primary and dependency handles, opens dependencies before their consumer, applies requested global visibility, and
initializes modules. This matters especially on ELF systems, where a later library may need symbols from an earlier
library opened globally. A permissive macOS loader is insufficient validation.

Manifest-backed `exported` checks the requested Rust ABI against trusted signature metadata, verifies function/global
classification and symbol presence, then creates the typed handle. The manifest is cooperative build metadata, not
protection against malicious libraries or substituted native code.

`LeanCapability::from_manifest_snapshot` consumes captured bytes without rereading the manifest path. The application
keeps the artifacts unchanged through loading and initializers; a storage descriptor alone is not evidence of the loaded
ABI. Worker sessions follow [Prepared host artifacts](worker-sessions.md#prepared-host-artifacts).

`LeanCapabilityPreflight` and `LeanRuntimePreflight` expose doctor-style checks. `LeanLibrary::open`,
`LeanLibrary::open_globally`, direct dylib descriptors, and unchecked exports remain advanced paths whose callers take
on the corresponding loader or ABI obligations.

## Native ownership in 0.10

In 0.9.9, a caller could open a library or manifest, bind a manifest-declared export, and call it from safe Rust. In
0.10, opening native code is `unsafe`. Loader constructors can run as soon as an image opens. Before loading, the
application must establish that the primary image, dependencies, constructors and transitive initializers are safe for
the actual runtime, including any registrations they leave behind. Generic export binding and invocation also require
`unsafe`. The application checks the native signature, boxed layouts, ownership and callback protocol. A typed binding
such as `TrustedWorkerCapability::bind_binary` lets later callers use the contract the application admitted.

Ordinary `LeanLibrary` and `LeanCapability` handles own scoped mappings. Keep every required image mapped while a
returned Lean value, callback or global registration can use its code or data. Trusted host and worker capabilities pin
their admitted bundles until process exit. When loading a trusted worker snapshot, the SDK pins each configured
initializer's image and all earlier explicit dependencies before attempting that initializer. A later failure can leave
a registration live. Passing an already initialized bundle through `TrustedWorkerCapability::from_loaded` also retains
its images if the export allowlist is rejected.

If a call through `LeanLibrary::initialize_module` enters a native initializer and returns an error, the SDK keeps the
image and one failure entry for that resolved initializer address until process exit. Another handle or symbol alias
resolving to the same address gets the original error. A simultaneous call to an initializer already running gets a load
error. The application still coordinates external and transitive initialization, including failures inside an
initializer that calls another directly.

Repeated pins of the same live loader handle share one retained mapping. Distinct pinned images and failed initializer
addresses have no automatic limit or eviction. Process exit releases them. A pin keeps code and data addressable. The
application separately keeps artifacts stable, including inputs for deferred imports and the time between verification
and loading. A foreign unwind or abort has no in-process recovery guarantee.

## Worker bootstrap

Applications loading native capabilities ship a child that calls `lean_rs_worker_child::run_worker_child_with_authority`
with their own admission policy. `LeanWorkerChild` locates that binary and binds it to the matching Lean sysroot. The
child's linked `libleanshared` and runtime stdlib oleans must come from the same toolchain.

A manifest-backed worker session opens the same artifact inside the child, and the supervisor hides pipes, framing, and
bootstrap diagnostics. Typed child settings carry supported configuration. The low-level supervisor inherits the
parent's environment and applies worker-specific overrides; it does not claim to sanitize the environment. Normal
packaged examples should not require callers to set dynamic-loader variables or open Lean dylibs manually.

`LeanWorkerHostHandleBuilder::check()` validates the worker child locator, the protocol handshake, and a shims-only
session open before a body command. Current import startup still needs Lean tooling on `PATH`; the report exposes that
deployment requirement. Process isolation and memory-reset semantics remain with the
[worker boundary](worker-processes.md).

## Package and regression checks

Published tarballs must include every source, shim, template, and metadata file their build path needs. docs.rs builds
must work with Lean, Lake, and Elan unavailable. `scripts/check_package_docsrs.py` checks package contents and simulates
docs.rs from normalized tarballs with those tools hidden from `PATH`; a docs.rs build-script guard alone does not check
package completeness.

`crates/lean-rs-worker-child/tests/loader_regressions.rs` checks the shipped template's package list, transitive
capability lifetime, same-process binary, and worker example. It removes `LD_LIBRARY_PATH`, `LD_PRELOAD`,
`DYLD_LIBRARY_PATH`, `DYLD_FALLBACK_LIBRARY_PATH`, and `DYLD_INSERT_LIBRARIES` to test artifact/rpath/bundle behavior
without a developer shell's loader setup. It keeps `PATH` for the current tooling requirement.

These gates protect three distinct contracts: build artifacts describe dependencies, the runtime anchors and loads them,
and the worker boots the application-owned child with typed diagnostics.
