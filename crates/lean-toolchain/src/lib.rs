//! Lean 4 toolchain discovery, fingerprinting, allowlist re-export, and build-script helpers.
//!
//! Sits one layer above [`lean_rs_abi`], which owns link-free ABI/toolchain metadata. This crate
//! composes on top: a typed [`ToolchainFingerprint`], a layered [`LinkDiagnostics`] error type,
//! [`LakeWorkspace`], which asks Lake for import roots and library paths, and build-script
//! helpers ([`emit_lean_link_directives_checked`] and [`CargoLeanCapability`]) that print
//! consistent link and rerun directives.
//!
//! ## Single typed entry point
//!
//! [`LEAN_VERSION`], [`LEAN_HEADER_PATH`], [`LEAN_HEADER_DIGEST`], and [`LEAN_RESOLVED_VERSION`]
//! are resolved by this crate's `build.rs` (probing the active toolchain, degrading to the latest
//! supported entry when none is installed), so embedders need only one import for build metadata.
//! Live toolchain identity lives here, not in [`lean_rs_abi`], which is purely static.
//!
//! ## Layering
//!
//! `lean-rs-abi → lean-toolchain` for link-free metadata, and `lean-rs-sys → lean-rs`
//! for raw runtime FFI. Raw `lean_*` symbols never appear in this crate's public surface.

#![deny(unsafe_code)]

mod build_helpers;
mod built_capability;
mod diagnostics;
mod discover;
mod fingerprint;
pub mod host_shim_abi;
mod lake;
mod limits;
mod loader;
pub mod manifest_validation;
#[cfg(unix)]
pub mod process_group;
mod source_package;
#[cfg(test)]
mod test_dir;

pub use build_helpers::{
    BuiltLeanCapability, CAPABILITY_MANIFEST_SCHEMA_VERSION, CargoLeanCapability, emit_lean_link_directives_checked,
    generated_output_key_for_sysroot,
};
pub use built_capability::{BuiltCapabilityArtifact, LeanBuiltCapability, LeanBuiltCapabilityError};
pub use diagnostics::LinkDiagnostics;
pub use discover::{DiscoverOptions, DiscoverySource, ToolchainInfo, discover_toolchain};
pub use fingerprint::{
    LEAN_HEADER_DIGEST, LEAN_HEADER_PATH, LEAN_RESOLVED_VERSION, LEAN_VERSION, ToolchainFingerprint,
};
pub use lake::LakeWorkspace;
pub use lean_rs_abi::{SUPPORTED_TOOLCHAINS, SupportedToolchain, supported_by_digest, supported_for};
pub use limits::{
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX, LEAN_HEARTBEAT_LIMIT_DEFAULT,
    LEAN_HEARTBEAT_LIMIT_MAX,
};
pub use loader::{
    LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
    LeanExportSignature, LeanExportSymbolKind, LeanLibraryDependency, LeanLoaderCheck, LeanLoaderDiagnosticCode,
    LeanLoaderReport, LeanLoaderSeverity, LeanModuleInitializer,
};
pub use manifest_validation::CapabilityManifest;
pub use source_package::{
    GeneratedSourceFile, MaterializedSourcePackage, SourcePackageError, SourcePackageMaterializationRequest,
    SourcePackageProvenance, materialize_source_package, with_materialized_source_package,
};

/// Version of the `lean-toolchain` crate, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::{LEAN_HEADER_DIGEST, LEAN_VERSION, VERSION};

    #[test]
    fn version_constant_matches_package() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn lean_metadata_is_baked() {
        assert!(!LEAN_VERSION.is_empty());
        assert_eq!(LEAN_HEADER_DIGEST.len(), 64);
        assert!(LEAN_HEADER_DIGEST.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

mod lean_options;
pub use lean_options::{LeanOptionValue, lean_options_json};
