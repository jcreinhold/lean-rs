//! Lean 4 toolchain discovery, fingerprinting, allowlist re-export, and build-script helpers.
//!
//! Sits one layer above [`lean_rs_abi`], which owns link-free ABI/toolchain metadata. This crate
//! composes on top: a typed [`ToolchainFingerprint`], the workspace-only Lake [`LAKE_FIXTURE_DIGEST`], a layered
//! [`LinkDiagnostics`] error type, and reusable build-script helpers
//! ([`emit_lean_link_directives_checked`], [`build_lake_target`],
//! [`build_lake_target_quiet`]) that downstream embedders and higher layers can use to get
//! consistent link/rerun directives and Lake dylib paths without duplicating policy.
//!
//! ## Single typed entry point
//!
//! [`LEAN_VERSION`], [`LEAN_HEADER_PATH`], and [`LEAN_HEADER_DIGEST`] are re-exported from
//! [`lean_rs_abi`] so embedders that depend on this crate need only one import for build
//! metadata. The allowlist comes through [`required_symbols`] (no copy).
//!
//! ## Layering
//!
//! `lean-rs-abi → lean-toolchain` for link-free metadata, and `lean-rs-sys → lean-rs`
//! for raw runtime FFI. Raw `lean_*` symbols never appear in this crate's public surface.

#![forbid(unsafe_code)]

mod build_helpers;
mod built_capability;
mod diagnostics;
mod discover;
mod fingerprint;
mod lakefile_toml;
mod limits;
mod loader;
pub mod manifest_validation;
mod modules;
mod source_package;

pub use build_helpers::{
    BuiltLeanCapability, CAPABILITY_MANIFEST_SCHEMA_VERSION, CargoLeanCapability, build_lake_target,
    build_lake_target_quiet, capability_env_var, capability_manifest_env_var, emit_lean_link_directives,
    emit_lean_link_directives_checked,
};
pub use built_capability::{BuiltCapabilityArtifact, LeanBuiltCapability, LeanBuiltCapabilityError};
pub use diagnostics::LinkDiagnostics;
pub use discover::{DiscoverOptions, DiscoverySource, ToolchainInfo, discover_toolchain};
pub use fingerprint::{HOST_TRIPLE, LAKE_FIXTURE_DIGEST, ToolchainFingerprint};
pub use lean_rs_abi::{LEAN_HEADER_DIGEST, LEAN_HEADER_PATH, LEAN_RESOLVED_VERSION, LEAN_VERSION};
pub use lean_rs_abi::{SUPPORTED_TOOLCHAINS, SupportedToolchain, supported_by_digest, supported_for};
pub use limits::{
    LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX, LEAN_HEARTBEAT_LIMIT_DEFAULT,
    LEAN_HEARTBEAT_LIMIT_MAX,
};
pub use loader::{
    LOADER_DIAGNOSTIC_TEXT_LIMIT, LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention,
    LeanExportReturnAbi, LeanExportSignature, LeanExportSymbolKind, LeanLibraryDependency, LeanLoaderCheck,
    LeanLoaderDiagnosticCode, LeanLoaderReport, LeanLoaderSeverity, LeanModuleInitializer, bound_loader_text,
};
pub use manifest_validation::CapabilityManifest;
pub use modules::{
    LeanLakeProjectModules, LeanModuleDescriptor, LeanModuleDiscoveryDiagnostic, LeanModuleDiscoveryOptions,
    LeanModuleSetFingerprint, discover_lake_modules,
};
pub use source_package::{
    GeneratedSourceFile, MaterializedSourcePackage, SourcePackageError, SourcePackageMaterializationRequest,
    SourcePackageProvenance, materialize_source_package,
};

/// Version of the `lean-toolchain` crate, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Curated allowlist of `LEAN_EXPORT` symbols the workspace relies on.
///
/// Returns [`lean_rs_abi::REQUIRED_SYMBOLS`] directly—the allowlist lives in
/// exactly one place. Use this through `lean-toolchain` so consumer crates do
/// not also need a direct raw-FFI dependency just to enumerate symbol
/// names.
#[must_use]
pub fn required_symbols() -> &'static [&'static str] {
    lean_rs_abi::REQUIRED_SYMBOLS
}

#[cfg(test)]
mod tests {
    use super::{LEAN_HEADER_DIGEST, LEAN_VERSION, VERSION, required_symbols};

    #[test]
    fn version_constant_matches_package() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn required_symbols_is_nonempty() {
        assert!(!required_symbols().is_empty());
    }

    #[test]
    fn lean_metadata_is_baked() {
        assert!(!LEAN_VERSION.is_empty());
        assert_eq!(LEAN_HEADER_DIGEST.len(), 64);
        assert!(LEAN_HEADER_DIGEST.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
