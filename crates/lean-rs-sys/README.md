# lean-rs-sys

Raw bindings to the Lean 4 C ABI. It sits between [`lean-rs-abi`](https://docs.rs/lean-rs-abi), which holds the ABI
facts and needs no Lean, and [`lean-rs`](https://docs.rs/lean-rs), the safe layer that most code should use.

Public types are opaque: `lean_object` is zero-sized and `!Send + !Sync + !Unpin`, and code outside the crate reaches
reference counts, tags and payloads only through `pub unsafe fn` helpers, each with a `# Safety` section. Calling a Lean
runtime function is `unsafe`; reading static metadata is safe. Lean's object layout is not part of this crate's semver.

The build script finds Lean through `LEAN_SYSROOT` or `lean --print-prefix`, checks the digest of the active `lean.h`
against the supported window, and emits the link directives; a digest outside the window fails the build. The default
features (`dynamic`, `mimalloc`) link `libleanshared`. With `DOCS_RS=1` the script skips discovery and linking, so the
docs build without Lean.

The supported Lean releases are in the [version matrix](https://github.com/jcreinhold/lean-rs#version-matrix).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
