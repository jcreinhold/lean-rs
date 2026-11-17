# Loader and Artifact Boundary

Shipped capabilities use one build-time artifact description for same-process loading and worker startup:

```text
CargoLeanCapability -> manifest-backed LeanBuiltCapability
  -> LeanCapability or LeanWorkerCapabilityBuilder / LeanWorkerPool
```

Applications supply their Lake package/module, exported commands, schemas, and isolation policy. The library owns Lake
artifact naming, dependency load order, symbol visibility, handle lifetime, module initialization, preflight, and worker
bootstrap. Repeating those details in each consumer's build script or loader would produce platform-specific failures.

## Build artifacts

`lean-toolchain::CargoLeanCapability` builds the shared Lake target, emits Cargo rerun/link directives, and writes a
manifest containing the primary dylib, declared dependency dylibs, initializer metadata, toolchain fingerprint, and
trusted export signatures. `LeanBuiltCapability` is the link-free descriptor shared by the runtime and worker parent.

Declare separately built dependency dylibs through the builder so both loading paths see the same order and visibility
requirements. `build_lake_target` and raw link directives remain available for custom build systems; the
[shipped-crate template](https://github.com/jcreinhold/lean-rs/tree/main/templates/shipped-lean-crate) demonstrates the normal path. Shared build coordination
belongs to the [build-and-link contract](12-interop-build-and-link.md).

## Runtime loading

`LeanCapability::from_build_manifest` preflights the artifact and opens its `LeanLibraryBundle`. The bundle retains the
primary and dependency handles, opens dependencies before their consumer, applies requested global visibility, and
initializes modules. This matters especially on ELF systems, where a later library may need symbols from an earlier
library opened globally. A permissive macOS loader is insufficient validation.

Manifest-backed `exported` checks the requested Rust ABI against trusted signature metadata, verifies function/global
classification and symbol presence, then creates the typed handle. The manifest is cooperative build metadata, not
protection against malicious libraries or substituted native code.

`LeanCapability::from_manifest_snapshot` consumes captured bytes without rereading the manifest path. Artifact custody
must span loading and initializers; a storage descriptor alone is not evidence of the loaded ABI. Guarded worker
sessions apply the [read-custody protocol](17-worker-session-adapter.md#shared-artifact-read-custody).

`LeanCapabilityPreflight` and `LeanRuntimePreflight` expose doctor-style checks. `LeanLibrary::open`,
`LeanLibrary::open_globally`, direct dylib descriptors, and unchecked exports remain advanced paths whose callers take
on the corresponding loader or ABI obligations.

## Worker bootstrap

Applications ship an app-owned child calling `lean_rs_worker_child::run_worker_child_stdio`; dependency binaries are not
automatically installed with a consuming application. `LeanWorkerChild` locates that binary and binds it to the matching
Lean sysroot. The child's linked `libleanshared` and runtime stdlib oleans must come from the same toolchain.

The capability builder opens the same manifest-backed artifact inside the child and hides pipes, framing, and bootstrap
diagnostics. Typed child settings carry supported configuration. The low-level supervisor inherits the parent's
environment and applies worker-specific overrides; it does not claim to sanitize the environment. Normal packaged
examples should not require callers to set dynamic-loader variables or open Lean dylibs manually.

`LeanWorkerCapabilityBuilder::check()` validates child resolution/executable status, artifact preflight, handshake,
import startup, and optional metadata expectations before a body command. Current import startup still needs Lean
tooling on `PATH`; the report exposes that deployment requirement. Process isolation and memory-reset semantics remain
with the [worker boundary](16-production-boundary.md).

## Package and regression checks

Published tarballs must include every source, shim, template, and metadata file their build path needs. docs.rs builds
must work with Lean, Lake, and Elan unavailable. `scripts/check_package_docsrs.py` checks package contents and simulates
docs.rs from normalized tarballs with those tools hidden from `PATH`; a docs.rs build-script guard alone does not check
package completeness.

`crates/lean-rs-worker-child/tests/loader_regressions.rs` checks the shipped template's package list, transitive
capability lifetime, same-process binary, and worker example. It removes `LD_LIBRARY_PATH`, `LD_PRELOAD`,
`DYLD_LIBRARY_PATH`, `DYLD_FALLBACK_LIBRARY_PATH`, and `DYLD_INSERT_LIBRARIES` to test artifact/rpath/bundle behavior
without a developer shell's loader setup. It keeps `PATH` for the current tooling requirement.

These gates protect three distinct contracts: build artifacts describe dependencies, the runtime anchors and loads
them, and the worker boots the application-owned child with typed diagnostics.
