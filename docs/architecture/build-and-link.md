# Interop Build And Link

`lean-toolchain` owns the build-script path for downstream crates that compile Lake shared-library targets. Callers name
the Lake project root and target; they do not construct `.lake/build/lib` paths or reimplement Lake's package-name
mangling.

## Build Script Contract

Every build call acquires writer custody before cache inspection, including cache hits. Bind a shared-writer helper
such as the standalone `lean-rs-writer` executable by canonical path
with `.writer_executable(...)` or `LEAN_HOST_MCP_WRITER_EXECUTABLE`. A crate may instead select `PrivateLakeBootstrap`
when its Lean root and all dependencies lie strictly below the owning Cargo package:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let owner = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let lean_root = owner.join("lean");
    let bootstrap = lean_toolchain::shared_writer::PrivateLakeBootstrap::new(&owner, &lean_root)?;
    lean_toolchain::CargoLeanCapability::new(&lean_root, "MyCapability")
        .private_bootstrap(bootstrap)
        .package("my_app")
        .module("MyCapability")
        .build()?;
    Ok(())
}
```

The helper emits the Lean link-search, link-lib, runtime rpath, Cargo rerun directives, a deterministic manifest
compile-time environment variable, and a compatibility dylib-path variable:

```text
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST=<manifest path>
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB=<built dylib path>
```

It runs `lake build MyCapability:shared`, resolves Lake's supported dylib naming conventions, writes a self-describing
artifact manifest, and returns `BuiltLeanCapability` for build scripts that want to inspect the result. The manifest is
the canonical runtime handoff for shipped crates; the direct dylib path remains a lower-level compatibility path.

For a dependency outside the Cargo owner, use the configured writer rather than a package-local bootstrap. Record
separately built dependency libraries in the artifact manifest. Hosts selecting their own Lean toolchain can also pass
the matching sysroot:

```rust,ignore
use lean_toolchain::LeanLibraryDependency;

lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
    .writer_executable(configured_writer)
    .package("my_app")
    .module("MyCapability")
    .dependency(
        LeanLibraryDependency::path(dependency_dylib)
            .export_symbols_for_dependents()
            .initializer("dependency_pkg", "Dependency"),
    )
    .lean_sysroot(consumer_lean_sysroot)
    .build()?;
```

The dependency and explicit sysroot are optional. Dependency metadata tells the loader what to open; the owning crate
still decides how sources are packaged, the Lake package/module names, and which exports are trusted.

The selected sysroot is scoped to this helper call. Link-directive discovery uses that prefix, and the Lake build runs
`<sysroot>/bin/lake build <target>:shared` with `LEAN_SYSROOT` set only on the child command. The helper does not set
process-global environment variables or expose a raw `LEAN_PATH` override.

Lower-level helpers remain available for custom build flows:

- `emit_lean_link_directives_checked` emits only Lean link directives and returns typed `LinkDiagnostics` on discovery
  failure.
- `emit_lean_link_directives` preserves the warning-on-failure behavior.
- `build_lake_target` builds one Lake shared-library target and emits Cargo rerun directives.
- `build_lake_target_quiet` uses the same cache and path resolver without emitting Cargo directives. The host's bundled
  shim path uses `CargoLeanCapability::build_quiet` so its manifest also carries trusted export signatures.

`build_lake_target` emits `cargo:rerun-if-changed` lines for the Lake files and Lean source files it scans. It captures
Lake stdout and stderr, so stdout stays limited to Cargo directives and caller-chosen `cargo:` lines.

`discover_lake_modules` is the runtime discovery companion for higher-level planners. It resolves the Lake root,
discovers `lean_lib` module roots, enumerates source modules deterministically, and returns source-set fingerprints
without building a shared library. The worker crates use that general discovery output for import-set planning;
downstream crates should not reimplement Lake source-root or module-path conventions.

Human cache diagnostics go to stderr:

```text
lean-toolchain: cache hit for Lake target `LeanRsInterop` in ...; using ...
lean-toolchain: cache miss for Lake target `LeanRsInterop` in ...; running `lake build LeanRsInterop:shared`
```

## Cache Key

The version 2 cache key covers the Lake project and its bounded declared dependency closure:

- SHA-256 of `lake-manifest.json`;
- target name and Lake package name;
- explicit Lean sysroot path, or the ambient-toolchain selector;
- count of scanned source files;
- maximum mtime of `lakefile.lean`, `lakefile.toml`, `lean-toolchain`, and every `*.lean` file below the project root,
  excluding `.lake/`;
- streaming content SHA-256 of the primary project and each canonical dependency's supported owned file tree, excluding
  `.lake` and `.git`,
  including C/header/resource inputs, Lake configuration, toolchain pin, manifest and supported override metadata; this
  detects content changes even when size and modification timestamps are preserved;
- file and directory identity observations, including Unix device, inode and change time, which also detect ordinary
  restored edits and transient source creation/removal; non-Unix platforms lack that Unix change-time evidence;
- content SHA-256 of dependent dylibs recorded by the capability builder, excluding the target's own output.

Before cache publication the complete source input fingerprint must still equal the pre-build fingerprint. A changed
input set refuses publication rather than stamping new source identity onto an artifact built from older inputs. Each
streamed file and traversed directory must also retain its observation while the fingerprint is taken. This is bounded
workspace observation, not an immutable filesystem snapshot or protection against arbitrary external I/O. The writer
normalizes the standard `.lake` output directory before its initial source observation. Lake may rebuild dependency
outputs during a successful command; their final content trace is saved for later cache checks. Earlier cache versions
miss once and rebuild through ordinary writer custody.

A hit is accepted only when the cache key matches and the expected dylib exists. Cache write, manifest, traversal, and
output-resolution failures are `LinkDiagnostics::LakeOutputUnresolved`.

After the target succeeds, `CargoLeanCapability` checks each dependency library before publishing the capability
manifest. A separately built dependency that is still missing yields `LakeOutputUnresolved` with its path. Under
shared reader custody, `probe_ready()` reports a cache miss for the missing file. Release reader custody before
building the missing dependency and refreshing the capability.

## Writer Custody

`shared_writer::project_lock_path(root)` resolves one sibling lock outside the entire project root. It canonicalizes
existing aliases, or the existing parent plus one final basename for a package that has not been materialized. Replacing
`.lake` or that package root therefore preserves its coordination domain. Removing or renaming the parent requires the
same writer authority; a live lock file must never be unlinked.

`shared_writer::project_artifact_roots(root)` is the common bounded reader/writer graph resolver. It uses the primary
Lake manifest's flattened workspace map, including inherited dependencies, and canonicalizes at most 32 artifact roots.
Dependency manifests validate membership in that map; Git packages are not guessed under each dependency's own
`.lake/packages`. Missing or unsupported metadata, nondefault Lake metadata directories, nonempty package overrides,
copied dependencies and Git layouts whose cleanup could unlink a nested domain in the declared graph are refused before
GO. A plain non-Lake fixture or proven zero-dependency payload needs only its primary domain. The immutable Lean sysroot
is a separate provider, outside these managed Lake domains.

Before any project EX attempt, managed writers announce compatible shared demand on stable sibling files for their
complete physical-domain union. Source publication announces only its project domain, never a cache-entry lock. Readers
briefly take exclusive admission gates before nonblocking artifact SH acquisition and release all gates before any
artifact wait. A writer-demand observation refuses a partial reader before its body rather than waiting while an earlier
SH remains held. Another transient reader gate permits bounded retry; a narrow release/probe race can conservatively
refuse a fresh reader. Neither demand nor its disappearance is evidence that any actual writer or native child stopped.
The configured helper retains its own compatible demand through writer cleanup; artifact EX aliases and the existing
process fences remain authoritative after helper or caller loss. Demand files are private, close-only kernel locks and
are never unlinked as recovery. Embedders retaining warm readers must fence matching new admissions and retire those
readers only after their admitted calls finish.

Whole-domain readers use `SharedReader::acquire_union` or `SharedReadProxy::acquire_union` before any body. The
complete canonical input union stays fixed; acquisition sorts lock paths but returns guards in input order. A complete
short gate preflight closes before helpers are spawned. Only opted-in normal helper exit 75, observed before any
transfer or readiness bytes, permits another admission round. The descriptor path rechecks its listener after reap;
the proxy path requires empty output and actual helper exit. Ordinary helper, transport, descriptor, identity and
cleanup errors remain hard, with bounded stage/status/stderr diagnostics. Every tentative descriptor closes first;
partial proxies all receive EOF and their actual helper exits are confirmed before waiting. One absolute deadline and
cancellation predicate cover all rounds. A warm owner retaining earlier guardian custody uses one-pass
`try_acquire_union` and must retire that generation on typed contention rather than wait on its own old locks.
No metadata, import, initializer, guardian acknowledgement or target body is replayed by these admission loops.

Writers try all project locks in globally sorted path order. Each pass independently opens each candidate, tries EX,
and checks prior writer/native ownership once. Contention or an explicit false ownership check closes the candidate
and every earlier ungranted artifact guard before any wait. Other errors propagate. The complete union's compatible
writer demand stays held across these retries; cancellation and the original time budget bound waiting. Zero timeout
still permits one immediate pass. Retrying never runs a command or turns an unknown outcome into success.

An initial **unlocked** file handle pins each target's original inode across retry passes. It is never flocked, cloned or
lent, and its descriptor closes on exec; every EX attempt opens a separate description without creating a replacement. Admission compares the
anchor, candidate and named inode before and after ownership checks. Anchors remain for domains present across a graph
refresh; removed domains close their anchors, matching the existing changed-graph restart policy. This is distinct from
keeping an ungranted EX lock. A caller may still own a separate cache-entry lock during project admission.

Writers revalidate the graph under the complete granted set and retry a changed graph only before launch within the
original admission budget, announcing the new complete union before another EX attempt. Returned loans name the primary
project first. Releasing retry candidates does not let ordinary readers bypass writer demand or grant permission to read
artifacts still in use; a recovery reader requires a separate reviewed path.

The helper validates every borrowed domain before
ACK and rechecks graph coverage and inode identity before each GO. Loan order still names the primary project first,
with up to 64 project/source-entry descriptions. `open_with_write_roots` also binds independently runnable prepared
workspaces. `project_artifact_root_union` validates their combined graph; `write_roots` exposes the held domains and
`covers_project` checks the current graph of a registered borrowed-build workspace. Commands must keep outputs inside
those declared domains or helper-owned assets. Arbitrary configuration I/O is not sandboxed. After EX acquisition,
borrowed-loan adoption, and immediately before GO, writers also inspect the common native ownership registry. An
unresolved generation protecting the same canonical logical domain blocks admission even when its guardian died and the
kernel lock became obtainable. A changed inode remains unresolved while that generation lives. Closed generations are
checked before resolving old paths, so removal of a closed generation's domain does not permanently fence writers. The
authoritative registry is supplied explicitly through `LEAN_RS_WORKER_OWNERS_DIRECTORY` or resolved to the shared
default. Parent native startup requires its lifetime namespace to match that lookup; managed writer helpers inherit the
same binding. Unsupported or aliased namespaces are refused rather than discovered by scanning unrelated locations.

A sibling lock requires a stable parent. An unrelated workspace that replaces an ancestor Git checkout can invalidate
nested project domains absent from its own graph; an independent project under a replaceable source-package root has the
same constraint. Cross-workspace replacement authority is not established by this graph-local union and remains an
unresolved safety limit.

`CargoLeanCapability` acquires the configured helper before cache inspection and retains it through Lake, cache
publication and `finish`'s capability manifest. Parent callers bind `.writer_executable(...)`; a worker may receive the
same explicit binding from its launcher. `build_quiet_with_writer(writer, cancelled)` reuses an already-owned scope.
`probe_ready()` is a read-only alternative under caller-held SH custody: it checks the same build-cache key and exact
expected capability manifest, including ABI exports, selected fingerprint and dependencies, without a runner, write or
publication. A miss returns `None`; each independently prepared dependency also needs its own probe. This lets a warm
actor load valid bundled artifacts while another live generation retains its shared read union. Generated
zero-dependency packages use `with_materialized_source_package_with_writer`: preflight the effective payload, acquire
the external entry and stable project locks, loan duplicated descriptions to one helper, await validated custody, then
materialize, build and publish. Caller copies remain close-only through local writes, including if the helper exits
unexpectedly. Every loan records actual command ownership before GO and is cleared only after confirmed closure. The
lexical callback API remains available; it does not independently establish descendant or controller-crash custody.

The helper owns its private input workspace. Cancellation, timeout and transport loss seal the scope and request
cleanup; callers wait only through bounded cleanup slack. On macOS the helper keeps its direct group leader unreaped
until the atomic kernel group census and inherited barrier establish closure, then reaps it. Managed commands must
retain their private PGID. The Linux backend is weaker: after leader reap it passively observes group absence and cannot
safely signal surviving descendants. Failed cleanup retains custody. Helper failure permits passive admission checks
only, never signaling from persisted numbers; a reused recorded group can conservatively delay admission.

`WriterFailure` preserves `NotStarted`, `Stopped`, or `Unknown` independently of proof/result bytes. Confirmed stop with
unusable output carries the actual terminal receipt; it never becomes a successful `Output`. `LakeWriterFailure`
preserves this typed cause through build diagnostics. An observer created before helper launch drains replies without
blocking on a discarded recipient and retains the helper's `Child` until reaped. This adds no recovery queue or daemon.

A typed `PrivateLakeBootstrap` is a separate Cargo bootstrap exception for bundled outputs below their Rust package. It
uses controller-held locks over its declared package-local dependency union and direct `Command::output`; it does not
gain the runtime helper's crash or process-tree guarantees. Ordinary unbound roots are rejected. Uncoordinated external
Lake commands remain outside this advisory protocol.

## Diagnostics

The helper path reports typed failures:

- `MissingLean`, `MissingHeader`, `MissingLib`, `UnsupportedToolchain`, and `VersionMismatch` from Lean toolchain
  discovery.
- `LakeUnavailable` when the `lake` executable cannot be started.
- `LakeWriterFailure` when owned writer admission, execution or cleanup fails, preserving its execution evidence.
- `LakeTargetMissing` when the target is not declared as a `lean_lib`.
- `LakeBuildFailed` when `lake build <target>:shared` exits unsuccessfully.
- `LakeOutputUnresolved` when the manifest, cache, source scan, or expected dylib path cannot be resolved.

Each diagnostic renders as one line, so callers may print it through `cargo:warning=` or return it from `build.rs`.

## Generic Interop Example

[`crates/lean-rs/examples/interop_callback.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/examples/interop_callback.rs) builds two Lake
targets through the helper:

- `crates/lean-rs/shims/lean-rs-interop-shims` target `LeanRsInterop`;
- `fixtures/interop-shims` target `LeanRsInteropConsumer`.

The example opens the generic shim dylib globally, opens the downstream-style consumer dylib, calls an ordinary Lean
export from Rust, and invokes a Lean callback loop through `LeanCallbackHandle`. It uses no `lean-rs-host` session API.

The example requires a configured `LEAN_HOST_MCP_WRITER_EXECUTABLE`; it does not select a helper itself. With that
prerequisite in place, run it against a cold cache and repeat to exercise the cache-hit path:

```sh
cargo run -p lean-rs --example interop_callback
cargo run -p lean-rs --example interop_callback
```

The canonical shipped-crate recipe is [`docs/recipes/ship-crate-with-lean.md`](../recipes/ship-crate-with-lean.md). The
advanced same-process callback recipe is [`docs/recipes/downstream-interop.md`](../recipes/downstream-interop.md).

## Scope

The helper builds Lake `lean_lib` shared facets for the host platform. It does not cross-compile, build executables, or
publish Lake packages. The generic and host shim sources are bundled with their owning Rust crates; this helper is the
path those crates and downstream examples use to materialize the dylibs.
