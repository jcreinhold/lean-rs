# lean-toolchain

Finds the Lean 4 toolchain and reports its identity for the `lean-rs` crates. It sits between
[`lean-rs-abi`](https://docs.rs/lean-rs-abi), which holds the ABI facts, and [`lean-rs`](https://docs.rs/lean-rs), and
owns the typed `ToolchainFingerprint`, link diagnostics, Lake queries and build-script helpers. Its build script bakes
in the live `LEAN_VERSION`, `LEAN_HEADER_PATH`, `LEAN_HEADER_DIGEST` and `LEAN_RESOLVED_VERSION`, using the newest
supported entry when no Lean is installed, and the crate re-exports `REQUIRED_SYMBOLS` and the supported window from
`lean-rs-abi`.

- `LakeWorkspace` asks Lake for a workspace's import roots (`lake env`) and for a library's shared file
  (`lake query <lib>:shared`, with or without a build). A caller that runs processes under its own time limit or process
  group can take each Lake command and hand back the output. Lake owns the package graph and the output layout; this
  crate reads no `lake-manifest.json` and computes no Lake output path.
- `CargoLeanCapability`, called from a build script, has Lake build a shared library, writes the library's artifact
  manifest, and emits the link directives and the manifest's path at compile time. It runs Lake directly and takes no
  lock.
- `process_group::OwnedCommandGroup` runs a command in its own process group and holds the group until it has gone. On
  macOS it waits for the whole group; on Linux, once it reaps the leader, it only checks whether the group still exists.

The supported Lean releases are in the [version matrix](https://github.com/jcreinhold/lean-rs#version-matrix).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
