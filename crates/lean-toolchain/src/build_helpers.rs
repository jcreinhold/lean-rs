//! Reusable build-script helpers for downstream embedders.
//!
//! Inside this workspace `lean-rs-sys` owns runtime link directives, while
//! `lean-rs-abi` owns link-free metadata. `lean-toolchain` does not call into
//! this helper from its own build script. The helper exists for **downstream
//! embedders** whose own `build.rs` would otherwise duplicate the link-policy
//! probe, the directive set, and the runtime rpath logic.
//!
//! Usage in a downstream `build.rs` for a shipped Rust-to-Lean capability:
//!
//! ```ignore
//! fn main() {
//!     lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
//!         .package("my_app")
//!         .module("MyCapability")
//!         .build()?;
//!     Ok::<(), Box<dyn std::error::Error>>(())
//! }
//! ```
//!
//! That one call covers link-time (the `cargo:rustc-link-search` /
//! `link-lib` directives), load-time (the rpath into the Lean toolchain's
//! `lib/lean` directory), and the build artifact manifest consumed by
//! `lean-rs` at runtime. A consumer binary should not need to construct Lake
//! output paths or set `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH`.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::diagnostics::LinkDiagnostics;
use crate::discover::{DiscoverOptions, ToolchainInfo, discover_toolchain};
use crate::fingerprint::ToolchainFingerprint;
use crate::lakefile_toml::parse_lakefile_toml;
use crate::loader::{LeanExportSignature, LeanLibraryDependency};

/// Generated-output slot selected by one exact supported Lean sysroot.
///
/// Official Lean 4.36.0 uses a versioned output slot. Earlier supported
/// releases keep their legacy paths. Selection checks both the parsed exact
/// version and the installed header digest; it does not run Lean or Lake.
///
/// # Errors
/// Returns discovery, header-read or unsupported-toolchain diagnostics.
pub fn generated_output_key_for_sysroot(sysroot: &Path) -> Result<Option<&'static str>, LinkDiagnostics> {
    let info = discover_toolchain(&DiscoverOptions {
        explicit_sysroot: Some(sysroot.to_owned()),
        allow_lean_sysroot_env: false,
        allow_path_lookup: false,
        allow_elan: false,
        allow_lake_env: false,
        toolchain_file: None,
    })?;
    let bytes = fs::read(&info.header_path).map_err(|_| LinkDiagnostics::MissingHeader {
        path: info.header_path.clone(),
    })?;
    use std::fmt::Write as _;
    let mut digest = String::new();
    for byte in Sha256::digest(bytes) {
        let formatted = write!(&mut digest, "{byte:02x}");
        debug_assert!(formatted.is_ok(), "formatting into a String cannot fail");
    }
    let Some(entry) = lean_rs_abi::supported_by_digest(&digest) else {
        return Err(LinkDiagnostics::UnsupportedToolchain {
            active: info.version,
            supported_window: supported_window(),
        });
    };
    if !entry.versions.contains(&info.version.as_str()) {
        return Err(LinkDiagnostics::UnsupportedToolchain {
            active: info.version,
            supported_window: supported_window(),
        });
    }
    Ok((info.version == "4.36.0").then_some("lean-4.36.0"))
}

/// Current JSON schema version for `CargoLeanCapability` artifact manifests.
pub const CAPABILITY_MANIFEST_SCHEMA_VERSION: u32 = 2;

/// Set once after a successful link-directive emission to make repeat calls
/// for the same toolchain cheap and idempotent.
static EMITTED_TOOLCHAIN_PREFIX: OnceLock<PathBuf> = OnceLock::new();

/// Emit Lean link-search, link-lib, and runtime rpath directives plus
/// matching rerun triggers from a downstream `build.rs`.
///
/// On the first call this:
///
/// 1. Runs [`discover_toolchain`] with [`DiscoverOptions::default()`].
/// 2. On success, prints `cargo:rustc-link-search=native=<prefix>/lib/lean`
///    and `<prefix>/lib`, plus `cargo:rustc-link-lib=dylib=leanshared`.
/// 3. On macOS or Linux build targets, also prints
///    `cargo:rustc-link-arg=-Wl,-rpath,<prefix>/lib/lean` so the resulting
///    binary loads `libleanshared` without `DYLD_FALLBACK_LIBRARY_PATH` /
///    `LD_LIBRARY_PATH`. Other targets get no rpath directive.
/// 4. On discovery failure, prints one `cargo:warning=` line with the
///    formatted diagnostic and returns; the caller's build then fails at
///    link time with a more specific error from rustc.
/// 5. Emits `cargo:rerun-if-changed=<header>` and the env-var triggers
///    discovery consults.
///
/// Subsequent calls within the same process are no-ops.
pub fn emit_lean_link_directives() {
    if let Err(diagnostic) = emit_lean_link_directives_checked() {
        println!("cargo:warning={diagnostic}");
    }
}

/// Emit Lean link-search, link-lib, and runtime rpath directives, returning
/// typed diagnostics if the active Lean toolchain cannot be resolved.
///
/// This is the build-script helper to use when the consumer wants `main() ->
/// Result<_, LinkDiagnostics>` or wants to map discovery failures into its own
/// error type. It emits the same `cargo:rustc-link-*`, rpath, and rerun
/// directives as [`emit_lean_link_directives`]. On failure, it still emits the
/// environment-variable rerun triggers discovery consulted, then returns the
/// [`LinkDiagnostics`] value instead of degrading it to `cargo:warning=`.
///
/// Subsequent successful calls within the same process are no-ops.
///
/// # Errors
///
/// Returns the diagnostics from [`discover_toolchain`] when Lean cannot be
/// found, the discovered prefix is malformed, or the active Lean version is
/// outside the supported window.
pub fn emit_lean_link_directives_checked() -> Result<(), LinkDiagnostics> {
    emit_lean_link_directives_checked_with_options(&DiscoverOptions::default()).map(drop)
}

fn emit_lean_link_directives_checked_with_options(opts: &DiscoverOptions) -> Result<ToolchainInfo, LinkDiagnostics> {
    let info = match discover_toolchain(opts) {
        Ok(info) => info,
        Err(diagnostic) => {
            emit_rerun_triggers(None);
            return Err(diagnostic);
        }
    };

    if EMITTED_TOOLCHAIN_PREFIX
        .get()
        .is_some_and(|prefix| prefix == &info.prefix)
    {
        return Ok(info);
    }

    emit_for(&info);
    drop(EMITTED_TOOLCHAIN_PREFIX.set(info.prefix.clone()));
    Ok(info)
}

/// Build a Lake `lean_lib` shared-library target and return the produced dylib path.
///
/// `project_root` must be the directory containing the project's lakefile—
/// either `lakefile.lean` (Lean DSL) or `lakefile.toml`. `target_name` is the
/// Lake target name to build; the helper invokes `lake build <target_name>:shared` on a
/// cache miss and returns the supported-window dylib path under
/// `<project_root>/.lake/build/lib/`.
///
/// The cache key is:
///
/// - SHA-256 of `lake-manifest.json`, or `missing` until Lake creates one;
/// - the maximum modification timestamp of `lakefile.lean`, `lakefile.toml`, `lean-toolchain`,
///   and every `*.lean` file below `project_root` excluding `.lake/`;
/// - the counted source-set size;
/// - the target name and Lake package name;
/// - content SHA-256 of supported owned source trees across the bounded
///   declared workspace closure, including C/header/resource inputs and
///   excluding `.lake` and `.git` artifacts;
/// - content SHA-256 of configured dependent shared libraries.
///
/// Version 2 invalidates earlier keys. Inputs are fingerprinted before and after
/// a build; a changed input set refuses cache/manifest publication. Output hashes
/// are recorded after a successful build, allowing Lake to build dependencies.
/// A cache hit skips the Lake command only when the cache key matches and the dylib exists.
/// The helper always emits `cargo:rerun-if-changed=...` directives for the Lake files and
/// source files it scans. If `lake-manifest.json` is absent, the helper lets
/// `lake build` create it. It captures Lake stdout/stderr and never forwards Lake output to
/// stdout, so stdout remains valid Cargo build-script directives only.
///
/// # Errors
///
/// Returns [`LinkDiagnostics::LakeTargetMissing`] if `target_name` is not declared as a
/// `lean_lib` in the project's lakefile (`lakefile.lean` or `lakefile.toml`),
/// [`LinkDiagnostics::LakeBuildFailed`] if Lake exits
/// unsuccessfully, and [`LinkDiagnostics::LakeOutputUnresolved`] for unreadable manifests,
/// source-set traversal failures, cache write failures, or missing built dylibs.
pub fn build_lake_target(project_root: &Path, target_name: &str) -> Result<PathBuf, LinkDiagnostics> {
    let mut runner = RealLakeRunner::default();
    build_lake_target_with_runner_and_options(
        project_root,
        target_name,
        &mut runner,
        CargoMetadata::Emit,
        &LakeBuildOptions::default(),
    )
}

/// Build a Lake `lean_lib` shared-library target without emitting Cargo build-script directives.
///
/// This is the same Lake/cache resolver as [`build_lake_target`], but it writes no
/// `cargo:rerun-if-changed=...` lines to stdout. Use it from library/runtime code that needs
/// to materialize a bundled shim on demand. Build scripts should use [`build_lake_target`] so
/// Cargo sees the relevant rerun triggers.
///
/// # Errors
///
/// Returns the same [`LinkDiagnostics`] variants as [`build_lake_target`].
pub fn build_lake_target_quiet(project_root: &Path, target_name: &str) -> Result<PathBuf, LinkDiagnostics> {
    let mut runner = RealLakeRunner::default();
    build_lake_target_with_runner_and_options(
        project_root,
        target_name,
        &mut runner,
        CargoMetadata::Suppress,
        &LakeBuildOptions::default(),
    )
}

/// Build only the bundled package named by an explicit private bootstrap token.
///
/// # Errors
/// Returns the ordinary Lake build/cache diagnostics for that package.
pub fn build_private_lake_target_quiet(
    bootstrap: crate::shared_writer::PrivateLakeBootstrap,
    target_name: &str,
) -> Result<PathBuf, LinkDiagnostics> {
    let root = bootstrap.project_root().to_path_buf();
    let options = LakeBuildOptions {
        private_bootstrap: Some(bootstrap),
        ..LakeBuildOptions::default()
    };
    let mut runner = RealLakeRunner::default();
    build_lake_target_with_runner_and_options(&root, target_name, &mut runner, CargoMetadata::Suppress, &options)
}

/// Build-script helper for shipping a Rust crate with bundled Lean code.
///
/// This is the canonical downstream `build.rs` entry point. It composes
/// [`emit_lean_link_directives_checked`], [`build_lake_target`], and the
/// `cargo:rustc-env=...` directives that carry a JSON artifact manifest and a
/// backward-compatible dylib path into Rust code at compile time.
///
/// ```ignore
/// fn main() -> Result<(), Box<dyn std::error::Error>> {
///     lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
///         .package("my_app")
///         .module("MyCapability")
///         .build()?;
///     Ok(())
/// }
/// ```
#[derive(Clone, Debug)]
pub struct CargoLeanCapability {
    project_root: PathBuf,
    target_name: String,
    package: Option<String>,
    module: Option<String>,
    env_var: Option<String>,
    manifest_env_var: Option<String>,
    lean_sysroot: Option<PathBuf>,
    lean_num_threads: Option<u32>,
    generated_output_key: Option<String>,
    private_bootstrap: Option<crate::shared_writer::PrivateLakeBootstrap>,
    writer_executable: Option<PathBuf>,
    export_signatures: Vec<LeanExportSignature>,
    dependencies: Vec<LeanLibraryDependency>,
}

impl CargoLeanCapability {
    /// Create a build helper for a Lake project and `lean_lib` target.
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>, target_name: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
            target_name: target_name.into(),
            package: None,
            module: None,
            env_var: None,
            manifest_env_var: None,
            lean_sysroot: None,
            lean_num_threads: None,
            generated_output_key: None,
            private_bootstrap: None,
            writer_executable: None,
            export_signatures: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    /// Permit only a typed package-local bootstrap when no helper exists.
    #[must_use]
    pub fn private_bootstrap(mut self, bootstrap: crate::shared_writer::PrivateLakeBootstrap) -> Self {
        self.private_bootstrap = Some(bootstrap);
        self
    }

    /// Bind the shared-writer helper explicitly for parent-side builds.
    #[must_use]
    pub fn writer_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.writer_executable = Some(executable.into());
        self
    }

    /// Set the Lake package name used by the module initializer.
    ///
    /// If omitted, the helper infers the package from `lake-manifest.json` or
    /// the project's lakefile (`lakefile.lean` or `lakefile.toml`), matching
    /// [`build_lake_target`].
    #[must_use]
    pub fn package(mut self, package: impl Into<String>) -> Self {
        self.package = Some(package.into());
        self
    }

    /// Set the root Lean module name initialized by Rust.
    ///
    /// Defaults to the Lake target name.
    #[must_use]
    pub fn module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Override the generated Cargo environment variable name.
    ///
    /// The default is `LEAN_RS_CAPABILITY_<TARGET>_DYLIB`, with the target
    /// converted to screaming snake case.
    #[must_use]
    pub fn env_var(mut self, env_var: impl Into<String>) -> Self {
        self.env_var = Some(env_var.into());
        self
    }

    /// Override the generated Cargo environment variable name for the artifact
    /// manifest.
    ///
    /// The default is `LEAN_RS_CAPABILITY_<TARGET>_MANIFEST`, with the target
    /// converted to screaming snake case.
    #[must_use]
    pub fn manifest_env_var(mut self, env_var: impl Into<String>) -> Self {
        self.manifest_env_var = Some(env_var.into());
        self
    }

    /// Build and link against a specific Lean sysroot.
    ///
    /// `sysroot` is the Lean prefix containing `include/lean/lean.h` and,
    /// for real Lake builds, `bin/lake`. [`Self::build`] uses this sysroot
    /// for link-directive discovery. Both [`Self::build`] and
    /// [`Self::build_quiet`] pass it only to the spawned Lake command as
    /// `LEAN_SYSROOT` and run `<sysroot>/bin/lake`; they do not mutate the
    /// parent process environment.
    #[must_use]
    pub fn lean_sysroot(mut self, sysroot: impl Into<PathBuf>) -> Self {
        self.lean_sysroot = Some(sysroot.into());
        self
    }

    /// Cap the parallelism of the spawned `lake build` by setting `LEAN_NUM_THREADS`.
    ///
    /// Lake 5.x has no `--jobs` flag; it derives both the number of concurrent build jobs and
    /// each spawned `lean`'s task-manager thread count from `LEAN_NUM_THREADS`. Leaving this
    /// unset uses Lake's hardware-concurrency default, which fans out to one multi-GB `lean`
    /// process per core and can exhaust memory on large machines. Both [`Self::build`] and
    /// [`Self::build_quiet`] pass this only to the spawned Lake command; they do not mutate the
    /// parent process environment.
    #[must_use]
    pub fn lean_num_threads(mut self, threads: u32) -> Self {
        self.lean_num_threads = Some(threads);
        self
    }

    /// Select a distinct derived directory for generated Lake outputs.
    ///
    /// The key is one short ASCII path component. It cannot select an arbitrary
    /// path. Omitting it preserves Lake's existing `.lake/build` and SDK cache
    /// locations. This selects and validates the SDK's named dylib, cache and
    /// manifest paths; the Lake package must deterministically select the same
    /// build directory from its exact supported Lean version.
    #[must_use]
    pub fn generated_output_key(mut self, key: impl Into<String>) -> Self {
        self.generated_output_key = Some(key.into());
        self
    }

    /// Add trusted ABI metadata for one exported Lean symbol.
    ///
    /// The runtime checked-lookup API accepts a Rust call shape only when it
    /// exactly matches one of these manifest entries.
    #[must_use]
    pub fn export_signature(mut self, signature: LeanExportSignature) -> Self {
        self.export_signatures.push(signature);
        self
    }

    /// Add a dependent Lean dylib that must be loaded before this capability.
    ///
    /// Use this when the capability imports another shipped Lake package whose
    /// shared library was built separately. The dependency is recorded in the
    /// same artifact manifest consumed by `lean-rs` and the worker parent, so
    /// callers do not need to edit manifest JSON after the build.
    #[must_use]
    pub fn dependency(mut self, dependency: LeanLibraryDependency) -> Self {
        self.dependencies.push(dependency);
        self
    }

    /// Emit link directives, build the Lake shared library, write the
    /// artifact manifest, and emit `cargo:rustc-env` directives for the
    /// manifest and compatibility dylib path.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics`] if Lean cannot be discovered, Lake cannot
    /// build the target, or the target output cannot be resolved.
    pub fn build(self) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        self.build_with_runner(&mut RealLakeRunner::default(), CargoMetadata::Emit)
    }

    /// Same as [`Self::build`] without printing Cargo directives.
    ///
    /// This exists for tests and internal callers. Downstream `build.rs`
    /// scripts should use [`Self::build`].
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics`] if Lake cannot build the target or the
    /// target output cannot be resolved.
    pub fn build_quiet(self) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        self.build_with_runner(&mut RealLakeRunner::default(), CargoMetadata::Suppress)
    }

    /// Probe the existing build cache and exact capability manifest without writes.
    ///
    /// The caller must retain shared custody for the complete artifact-root union
    /// throughout this probe and subsequent loading. A miss never starts Lake or
    /// publishes metadata. Dependencies need their own cache probes when prepared
    /// independently; this probe also requires their recorded dylibs to exist.
    ///
    /// # Errors
    /// Returns malformed project, source traversal, toolchain or manifest errors.
    pub fn probe_ready(&self) -> Result<Option<BuiltLeanCapability>, LinkDiagnostics> {
        let root = match self.project_root.canonicalize() {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.probe_error(&error)),
        };
        let Some(lakefile) = existing_lakefile(&root) else {
            return Ok(None);
        };
        validate_generated_output_key(&root, &self.target_name, self.generated_output_key.as_deref())?;
        if !target_declared_in_lakefile(&lakefile, &self.target_name)? {
            return Ok(None);
        }
        let manifest_path = root.join("lake-manifest.json");
        let bytes = match fs::read(&manifest_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.probe_error(&error)),
        };
        let inferred = package_name_from_manifest(&root, &self.target_name, &manifest_path, &bytes)?;
        let options = LakeBuildOptions {
            lean_sysroot: self.lean_sysroot.clone(),
            lean_num_threads: self.lean_num_threads,
            generated_output_key: self.generated_output_key.clone(),
            private_bootstrap: self.private_bootstrap.clone(),
            writer_executable: self.writer_executable.clone(),
            dependencies: self.dependencies.clone(),
        };
        let key = cache_key(
            &self.target_name,
            &inferred,
            &sha256_hex(&bytes),
            &scan_source_set(&root, &self.target_name)?,
            &options,
            &build_inputs_fingerprint(&root, &self.target_name)?,
            &dependency_artifact_fingerprint(&root, &self.target_name, &options)?,
        );
        let cache = cache_path(&root, &self.target_name, &options);
        let dylib_path = resolve_dylib_path(&root, &inferred, &self.target_name, &options);
        let manifest_path = capability_manifest_path(&root, &self.target_name, &self.export_signatures, &options);
        if options.generated_output_key.is_some() {
            validate_generated_output_file(&root, &self.target_name, &cache)?;
            validate_generated_output_file(&root, &self.target_name, &dylib_path)?;
            validate_generated_output_file(&root, &self.target_name, &manifest_path)?;
        }
        if !fs::read_to_string(&cache).is_ok_and(|cached| cached == key) {
            return Ok(None);
        }
        if !dylib_path.is_file() {
            return Ok(None);
        }
        // Explicit discovery consults only the supplied sysroot, never PATH,
        // elan or Lake. With no explicit root build_quiet uses this fingerprint.
        let selected = if self.lean_sysroot.is_some() {
            Some(discover_toolchain(&self.discover_options())?)
        } else {
            None
        };
        let package = self.package.clone().unwrap_or(inferred);
        let module = self.module.clone().unwrap_or_else(|| self.target_name.clone());
        let env_var = self
            .env_var
            .clone()
            .unwrap_or_else(|| capability_env_var(&self.target_name));
        let manifest_env_var = self
            .manifest_env_var
            .clone()
            .unwrap_or_else(|| capability_manifest_env_var(&self.target_name));
        let expected = capability_manifest_value(
            &root,
            &self.target_name,
            &package,
            &module,
            &dylib_path,
            &manifest_env_var,
            &self.export_signatures,
            &self.dependencies,
            selected.as_ref(),
            &options,
        )?;
        let actual: serde_json::Value = match fs::read(&manifest_path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(_) => return Ok(None),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.probe_error(&error)),
        };
        if actual != expected {
            return Ok(None);
        }
        match check_manifest_dependency_files(&expected) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.probe_error(&error)),
        }
        Ok(Some(BuiltLeanCapability {
            dylib_path,
            env_var,
            manifest_path,
            manifest_env_var,
            package,
            module,
            target_name: self.target_name.clone(),
            project_root: root,
        }))
    }

    fn probe_error(&self, error: &io::Error) -> LinkDiagnostics {
        LinkDiagnostics::LakeOutputUnresolved {
            project_root: self.project_root.clone(),
            target_name: self.target_name.clone(),
            reason: format!("read-only cache probe failed ({error})"),
        }
    }

    /// Build using existing writer custody through cache and manifest publication.
    ///
    /// # Errors
    /// Returns cancellation, build, custody, or publication errors. Unknown work
    /// seals the supplied writer and never authorizes another command.
    #[cfg(unix)]
    pub fn build_quiet_with_writer(
        self,
        writer: &mut crate::shared_writer::SharedWriter,
        cancelled: impl Fn() -> bool,
    ) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        let mut runner = RealLakeRunner {
            borrowed: Some(writer),
            cancelled: Some(&cancelled),
            ..RealLakeRunner::default()
        };
        self.build_with_runner(&mut runner, CargoMetadata::Suppress)
    }

    /// Build with bounded cancellable lock acquisition.
    ///
    /// # Errors
    /// Returns acquisition, cancellation, build, or publication diagnostics.
    pub fn build_quiet_with_lock_wait(
        self,
        timeout: std::time::Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        let mut runner = RealLakeRunner {
            lock_timeout: Some(timeout),
            cancelled: Some(&cancelled),
            ..RealLakeRunner::default()
        };
        self.build_with_runner(&mut runner, CargoMetadata::Suppress)
    }

    fn build_with_runner(
        self,
        runner: &mut impl LakeRunner,
        cargo_metadata: CargoMetadata,
    ) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        let discover_options = self.discover_options();
        let selected_toolchain = match cargo_metadata {
            CargoMetadata::Emit => Some(emit_lean_link_directives_checked_with_options(&discover_options)?),
            CargoMetadata::Suppress if self.lean_sysroot.is_some() => Some(discover_toolchain(&discover_options)?),
            CargoMetadata::Suppress => None,
        };
        let lake_options = LakeBuildOptions {
            lean_sysroot: self.lean_sysroot.clone(),
            lean_num_threads: self.lean_num_threads,
            generated_output_key: self.generated_output_key.clone(),
            private_bootstrap: self.private_bootstrap.clone(),
            writer_executable: self.writer_executable.clone(),
            dependencies: self.dependencies.clone(),
        };
        let dylib_path = build_lake_target_with_runner_and_options(
            &self.project_root,
            &self.target_name,
            runner,
            cargo_metadata,
            &lake_options,
        )?;
        self.finish(dylib_path, cargo_metadata, selected_toolchain.as_ref(), &lake_options)
    }

    fn discover_options(&self) -> DiscoverOptions {
        let has_explicit_sysroot = self.lean_sysroot.is_some();
        DiscoverOptions {
            explicit_sysroot: self.lean_sysroot.clone(),
            allow_lean_sysroot_env: !has_explicit_sysroot,
            allow_path_lookup: !has_explicit_sysroot,
            allow_elan: !has_explicit_sysroot,
            allow_lake_env: !has_explicit_sysroot,
            toolchain_file: None,
        }
    }

    fn finish(
        self,
        dylib_path: PathBuf,
        cargo_metadata: CargoMetadata,
        selected_toolchain: Option<&ToolchainInfo>,
        options: &LakeBuildOptions,
    ) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        let project_root =
            fs::canonicalize(&self.project_root).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
                project_root: self.project_root.clone(),
                target_name: self.target_name.clone(),
                reason: format!(
                    "could not canonicalize project root {} ({err})",
                    self.project_root.display()
                ),
            })?;
        let package = match self.package {
            Some(package) => package,
            None => infer_package_name(&project_root, &self.target_name)?,
        };
        let module = self.module.unwrap_or_else(|| self.target_name.clone());
        let env_var = self.env_var.unwrap_or_else(|| capability_env_var(&self.target_name));
        let manifest_env_var = self
            .manifest_env_var
            .unwrap_or_else(|| capability_manifest_env_var(&self.target_name));
        let manifest_path = write_capability_manifest(
            &project_root,
            &self.target_name,
            &package,
            &module,
            &dylib_path,
            &manifest_env_var,
            &self.export_signatures,
            &self.dependencies,
            selected_toolchain,
            options,
        )?;
        cargo_metadata.println(format_args!("cargo:rustc-env={env_var}={}", dylib_path.display()));
        cargo_metadata.println(format_args!(
            "cargo:rustc-env={manifest_env_var}={}",
            manifest_path.display()
        ));
        Ok(BuiltLeanCapability {
            dylib_path,
            env_var,
            manifest_path,
            manifest_env_var,
            package,
            module,
            target_name: self.target_name,
            project_root,
        })
    }
}

/// Metadata produced by [`CargoLeanCapability`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltLeanCapability {
    dylib_path: PathBuf,
    env_var: String,
    manifest_path: PathBuf,
    manifest_env_var: String,
    package: String,
    module: String,
    target_name: String,
    project_root: PathBuf,
}

impl BuiltLeanCapability {
    /// Built shared-library path.
    #[must_use]
    pub fn dylib_path(&self) -> &Path {
        &self.dylib_path
    }

    /// Cargo environment variable that stores the built dylib path.
    #[must_use]
    pub fn env_var(&self) -> &str {
        &self.env_var
    }

    /// JSON artifact manifest path emitted by the build helper.
    #[must_use]
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Cargo environment variable that stores the artifact manifest path.
    #[must_use]
    pub fn manifest_env_var(&self) -> &str {
        &self.manifest_env_var
    }

    /// Lake package name.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Root Lean module initialized by Rust.
    #[must_use]
    pub fn module(&self) -> &str {
        &self.module
    }

    /// Lake target name.
    #[must_use]
    pub fn target_name(&self) -> &str {
        &self.target_name
    }

    /// Canonical Lake project root.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}

/// Default Cargo environment variable for a Lean capability target.
#[must_use]
pub fn capability_env_var(target_name: &str) -> String {
    format!("LEAN_RS_CAPABILITY_{}_DYLIB", screaming_snake(target_name))
}

/// Default Cargo environment variable for a Lean capability artifact manifest.
#[must_use]
pub fn capability_manifest_env_var(target_name: &str) -> String {
    format!("LEAN_RS_CAPABILITY_{}_MANIFEST", screaming_snake(target_name))
}

fn emit_for(info: &ToolchainInfo) {
    let lib_lean = info.lib_dir.join("lean");
    println!("cargo:rustc-link-search=native={}", lib_lean.display());
    println!("cargo:rustc-link-search=native={}", info.lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=leanshared");

    // Runtime rpath so the consumer binary finds `libleanshared` without
    // `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH`. Gated on the build
    // target (not the host) via `CARGO_CFG_TARGET_OS`; `-Wl,-rpath` is a
    // GNU-ld / lld / Apple-ld flag and is not meaningful on Windows.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "macos" | "linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_lean.display());
    }

    emit_rerun_triggers(Some(info));
}

fn emit_rerun_triggers(info: Option<&ToolchainInfo>) {
    if let Some(info) = info {
        println!("cargo:rerun-if-changed={}", info.header_path.display());
    }
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=ELAN_HOME");
    println!("cargo:rerun-if-env-changed=PATH");
}

trait LakeRunner {
    fn prepare(&mut self, _root: &Path, _options: &LakeBuildOptions) -> Result<(), std::io::Error> {
        Ok(())
    }
    fn build_shared(
        &mut self,
        project_root: &Path,
        target_name: &str,
        options: &LakeBuildOptions,
    ) -> Result<LakeRun, std::io::Error>;
}

fn writer_diagnostic(project_root: &Path, target_name: &str, error: &io::Error, admission: bool) -> LinkDiagnostics {
    if let Some(failure) = crate::shared_writer::WriterFailure::from_io(error) {
        return LinkDiagnostics::LakeWriterFailure {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            failure: Box::new(failure.clone()),
        };
    }
    if admission {
        LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not acquire shared Lake writer: {error}"),
        }
    } else {
        LinkDiagnostics::LakeUnavailable {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            detail: error.to_string(),
        }
    }
}

fn never_cancelled() -> bool {
    false
}

#[derive(Default)]
struct RealLakeRunner<'a> {
    root: Option<PathBuf>,
    writer: Option<crate::shared_writer::SharedWriter>,
    borrowed: Option<&'a mut crate::shared_writer::SharedWriter>,
    cancelled: Option<&'a dyn Fn() -> bool>,
    lock_timeout: Option<std::time::Duration>,
    private_locks: Vec<crate::writer_lock::WriterLock>,
}

impl LakeRunner for RealLakeRunner<'_> {
    fn prepare(&mut self, project_root: &Path, options: &LakeBuildOptions) -> Result<(), std::io::Error> {
        let root = project_root.canonicalize()?;
        if let Some(existing) = &self.root {
            return if existing == &root {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "one Lake build runner cannot cross writer roots",
                ))
            };
        }
        let cancelled: &dyn Fn() -> bool = self.cancelled.unwrap_or(&never_cancelled);
        let timeout = self.lock_timeout.unwrap_or(crate::writer_lock::DEFAULT_LOCK_TIMEOUT);
        if cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Lake build cancelled",
            ));
        }
        if let Some(writer) = &self.borrowed {
            if !writer.covers_project(&root)? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "borrowed writer does not cover this project",
                ));
            }
        } else if let Some(executable) = options
            .writer_executable
            .clone()
            .or_else(|| env::var_os(crate::shared_writer::SHARED_WRITER_EXECUTABLE_ENV).map(PathBuf::from))
        {
            self.writer = Some(crate::shared_writer::SharedWriter::open_cancellable(
                &executable,
                &root,
                timeout,
                cancelled,
            )?);
        } else if options
            .private_bootstrap
            .as_ref()
            .is_some_and(|mode| mode.project_root() == root)
        {
            let (locks, roots) = crate::writer_lock::WriterLock::acquire_projects(&root, &[], timeout, cancelled)?;
            if options
                .private_bootstrap
                .as_ref()
                .is_none_or(|mode| !mode.covers_roots(&roots))
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "private bootstrap dependency escaped its Cargo owner",
                ));
            }
            self.private_locks = locks;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "shared Lake writes require the configured writer helper; only typed package-local bootstraps are exempt",
            ));
        }
        self.root = Some(root);
        Ok(())
    }

    fn build_shared(
        &mut self,
        project_root: &Path,
        target_name: &str,
        options: &LakeBuildOptions,
    ) -> Result<LakeRun, std::io::Error> {
        let mut command = if let Some(sysroot) = options.lean_sysroot.as_deref() {
            let mut command = Command::new(sysroot.join("bin").join("lake"));
            command.env("LEAN_SYSROOT", sysroot);
            command
        } else {
            Command::new("lake")
        };
        // Lake 5.x has no `--jobs` flag; it derives both its build-job parallelism and each
        // spawned `lean`'s task-manager thread count from `LEAN_NUM_THREADS`. Set it explicitly
        // (rather than mutating the parent environment) so callers can bound a `lake build`
        // fan-out that would otherwise spawn one multi-GB `lean` per core.
        if let Some(threads) = options.lean_num_threads {
            command.env("LEAN_NUM_THREADS", threads.to_string());
        }
        command
            .arg("build")
            .arg(format!("{target_name}:shared"))
            .current_dir(project_root);
        self.prepare(project_root, options)?;
        let cancelled: &dyn Fn() -> bool = self.cancelled.unwrap_or(&never_cancelled);
        let output = if let Some(writer) = self.borrowed.as_deref_mut().or(self.writer.as_mut()) {
            writer.run_cancellable(&command, std::time::Duration::from_secs(120), cancelled)?
        } else {
            // A typed token proves this cwd is below its Rust package owner.
            command.output()?
        };
        Ok(LakeRun {
            success: output.status.success(),
            status: output.status.to_string(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LakeBuildOptions {
    lean_sysroot: Option<PathBuf>,
    /// Value for `LEAN_NUM_THREADS` on the spawned `lake` command. `None` leaves Lake's default
    /// (hardware-concurrency) parallelism, which can fork-storm a machine; `Some(n)` caps it.
    lean_num_threads: Option<u32>,
    generated_output_key: Option<String>,
    private_bootstrap: Option<crate::shared_writer::PrivateLakeBootstrap>,
    writer_executable: Option<PathBuf>,
    dependencies: Vec<LeanLibraryDependency>,
}

struct LakeRun {
    success: bool,
    status: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CargoMetadata {
    Emit,
    Suppress,
}

impl CargoMetadata {
    fn println(self, args: std::fmt::Arguments<'_>) {
        if matches!(self, Self::Emit) {
            println!("{args}");
        }
    }

    fn trace(self, args: std::fmt::Arguments<'_>) {
        if matches!(self, Self::Emit) {
            emit_lake_trace(args);
        }
    }
}

#[cfg(test)]
fn build_lake_target_with_runner(
    project_root: &Path,
    target_name: &str,
    runner: &mut impl LakeRunner,
    cargo_metadata: CargoMetadata,
) -> Result<PathBuf, LinkDiagnostics> {
    build_lake_target_with_runner_and_options(
        project_root,
        target_name,
        runner,
        cargo_metadata,
        &LakeBuildOptions::default(),
    )
}

fn build_lake_target_with_runner_and_options(
    project_root: &Path,
    target_name: &str,
    runner: &mut impl LakeRunner,
    cargo_metadata: CargoMetadata,
    options: &LakeBuildOptions,
) -> Result<PathBuf, LinkDiagnostics> {
    let project_root = fs::canonicalize(project_root).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not canonicalize project root {} ({err})", project_root.display()),
    })?;
    validate_generated_output_key(&project_root, target_name, options.generated_output_key.as_deref())?;
    runner
        .prepare(&project_root, options)
        .map_err(|error| writer_diagnostic(&project_root, target_name, &error, true))?;
    validate_generated_output_key(&project_root, target_name, options.generated_output_key.as_deref())?;
    let lakefile_lean = project_root.join("lakefile.lean");
    cargo_metadata.println(format_args!("cargo:rerun-if-changed={}", lakefile_lean.display()));
    let lakefile_toml = project_root.join("lakefile.toml");
    if lakefile_toml.is_file() {
        cargo_metadata.println(format_args!("cargo:rerun-if-changed={}", lakefile_toml.display()));
    }
    let toolchain_file = project_root.join("lean-toolchain");
    if toolchain_file.is_file() {
        cargo_metadata.println(format_args!("cargo:rerun-if-changed={}", toolchain_file.display()));
    }

    let lakefile = existing_lakefile(&project_root).ok_or_else(|| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.clone(),
        target_name: target_name.to_owned(),
        reason: format!(
            "no Lake lakefile found at {} or {}",
            lakefile_lean.display(),
            lakefile_toml.display()
        ),
    })?;

    if !target_declared_in_lakefile(&lakefile, target_name)? {
        return Err(LinkDiagnostics::LakeTargetMissing {
            project_root,
            target_name: target_name.to_owned(),
        });
    }

    let manifest_path = project_root.join("lake-manifest.json");
    cargo_metadata.println(format_args!("cargo:rerun-if-changed={}", manifest_path.display()));
    let (manifest_digest, package_name) = match fs::read(&manifest_path) {
        Ok(manifest_bytes) => (
            sha256_hex(&manifest_bytes),
            package_name_from_manifest(&project_root, target_name, &manifest_path, &manifest_bytes)?,
        ),
        Err(err) if err.kind() == io::ErrorKind::NotFound => (
            "missing".to_owned(),
            package_name_from_lakefile(&project_root, target_name, &lakefile)?,
        ),
        Err(err) => {
            return Err(LinkDiagnostics::LakeOutputUnresolved {
                project_root: project_root.clone(),
                target_name: target_name.to_owned(),
                reason: format!("could not read {} ({err})", manifest_path.display()),
            });
        }
    };
    let source_set = scan_source_set(&project_root, target_name)?;
    for path in &source_set.paths {
        cargo_metadata.println(format_args!("cargo:rerun-if-changed={}", path.display()));
    }

    let dylib = resolve_dylib_path(&project_root, &package_name, target_name, options);
    // All declared domains are already held by the runner. Normalize standard
    // output directories for the entire closure before source observation.
    for root in build_input_roots(&project_root, target_name)? {
        fs::create_dir_all(root.join(".lake")).map_err(|e| input_diagnostic(&project_root, target_name, &e))?;
    }
    let initial_inputs = build_inputs_fingerprint(&project_root, target_name)?;
    let initial_cache_key = cache_key(
        target_name,
        &package_name,
        &manifest_digest,
        &source_set,
        options,
        &initial_inputs,
        &dependency_artifact_fingerprint(&project_root, target_name, options)?,
    );
    let cache_path = cache_path(&project_root, target_name, options);
    if options.generated_output_key.is_some() {
        validate_generated_output_file(&project_root, target_name, &dylib)?;
        validate_generated_output_file(&project_root, target_name, &cache_path)?;
    }
    if dylib.is_file() && fs::read_to_string(&cache_path).is_ok_and(|cached| cached == initial_cache_key) {
        cargo_metadata.trace(format_args!(
            "lean-toolchain: cache hit for Lake target `{target_name}` in {}; using {}",
            project_root.display(),
            dylib.display(),
        ));
        return Ok(dylib);
    }
    cargo_metadata.trace(format_args!(
        "lean-toolchain: cache miss for Lake target `{target_name}` in {}; running `lake build {target_name}:shared`",
        project_root.display(),
    ));

    let run = runner
        .build_shared(&project_root, target_name, options)
        .map_err(|error| writer_diagnostic(&project_root, target_name, &error, false))?;
    if !run.success {
        return Err(LinkDiagnostics::LakeBuildFailed {
            project_root,
            target_name: target_name.to_owned(),
            status: run.status,
            detail: command_detail(&run.stdout, &run.stderr),
        });
    }

    let final_inputs = build_inputs_fingerprint(&project_root, target_name)?;
    if initial_inputs != final_inputs {
        return Err(LinkDiagnostics::LakeOutputUnresolved {
            project_root,
            target_name: target_name.to_owned(),
            reason: "Lake build inputs changed during execution; cache publication refused".to_owned(),
        });
    }
    let (final_manifest_digest, final_package_name) = match fs::read(&manifest_path) {
        Ok(manifest_bytes) => (
            sha256_hex(&manifest_bytes),
            package_name_from_manifest(&project_root, target_name, &manifest_path, &manifest_bytes)?,
        ),
        Err(_) => (manifest_digest, package_name),
    };
    let final_cache_key = cache_key(
        target_name,
        &final_package_name,
        &final_manifest_digest,
        &source_set,
        options,
        &final_inputs,
        &dependency_artifact_fingerprint(&project_root, target_name, options)?,
    );

    let dylib = resolve_dylib_path(&project_root, &final_package_name, target_name, options);
    if let Some(key) = options.generated_output_key.as_deref() {
        validate_generated_output_directories(&project_root, target_name, key)?;
        validate_generated_output_file(&project_root, target_name, &dylib)?;
        validate_generated_output_file(&project_root, target_name, &cache_path)?;
    }
    if !dylib.is_file() {
        return Err(LinkDiagnostics::LakeOutputUnresolved {
            project_root,
            target_name: target_name.to_owned(),
            reason: format!("expected shared library at {}", dylib.display()),
        });
    }

    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.clone(),
            target_name: target_name.to_owned(),
            reason: format!("could not create cache directory {} ({err})", parent.display()),
        })?;
    }
    let publish = if options.generated_output_key.is_some() {
        write_atomic(&cache_path, final_cache_key.as_bytes())
    } else {
        fs::write(&cache_path, final_cache_key)
    };
    publish.map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root,
        target_name: target_name.to_owned(),
        reason: format!("could not write cache file {} ({err})", cache_path.display()),
    })?;

    Ok(dylib)
}

fn target_declared_in_lakefile(lakefile: &Path, target_name: &str) -> Result<bool, LinkDiagnostics> {
    let contents = fs::read_to_string(lakefile).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: lakefile.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not read {} ({err})", lakefile.display()),
    })?;
    if lakefile_is_toml(lakefile) {
        let parsed = parse_lakefile_toml(&contents).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: lakefile.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("lakefile {} is not valid TOML ({err})", lakefile.display()),
        })?;
        return Ok(parsed.lean_libs.iter().any(|name| name == target_name));
    }
    let quoted = format!("lean_lib «{target_name}»");
    let bare = format!("lean_lib {target_name}");
    let string = format!("lean_lib \"{target_name}\"");
    Ok(contents.contains(&quoted) || contents.contains(&bare) || contents.contains(&string))
}

fn lakefile_is_toml(lakefile: &Path) -> bool {
    lakefile.file_name().and_then(|name| name.to_str()) == Some("lakefile.toml")
}

fn existing_lakefile(project_root: &Path) -> Option<PathBuf> {
    let toml = project_root.join("lakefile.toml");
    if toml.is_file() {
        return Some(toml);
    }
    let lean = project_root.join("lakefile.lean");
    lean.is_file().then_some(lean)
}

fn package_name_from_manifest(
    project_root: &Path,
    target_name: &str,
    manifest_path: &Path,
    manifest_bytes: &[u8],
) -> Result<String, LinkDiagnostics> {
    let manifest: serde_json::Value =
        serde_json::from_slice(manifest_bytes).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("{} is not valid JSON ({err})", manifest_path.display()),
        })?;
    manifest
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("{} has no string `name` field", manifest_path.display()),
        })
}

fn package_name_from_lakefile(
    project_root: &Path,
    target_name: &str,
    lakefile: &Path,
) -> Result<String, LinkDiagnostics> {
    let contents = fs::read_to_string(lakefile).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not read {} ({err})", lakefile.display()),
    })?;
    if lakefile_is_toml(lakefile) {
        let parsed = parse_lakefile_toml(&contents).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("lakefile {} is not valid TOML ({err})", lakefile.display()),
        })?;
        return parsed
            .package_name
            .ok_or_else(|| LinkDiagnostics::LakeOutputUnresolved {
                project_root: project_root.to_path_buf(),
                target_name: target_name.to_owned(),
                reason: format!("{} has no top-level `name` field", lakefile.display()),
            });
    }
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("package ") {
            return Ok(normalize_lake_identifier(rest));
        }
    }
    Err(LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("{} has no `package` declaration", lakefile.display()),
    })
}

fn infer_package_name(project_root: &Path, target_name: &str) -> Result<String, LinkDiagnostics> {
    let manifest_path = project_root.join("lake-manifest.json");
    match fs::read(&manifest_path) {
        Ok(manifest_bytes) => package_name_from_manifest(project_root, target_name, &manifest_path, &manifest_bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let lakefile = existing_lakefile(project_root).ok_or_else(|| LinkDiagnostics::LakeOutputUnresolved {
                project_root: project_root.to_path_buf(),
                target_name: target_name.to_owned(),
                reason: format!(
                    "no Lake lakefile found at {} or {}",
                    project_root.join("lakefile.lean").display(),
                    project_root.join("lakefile.toml").display()
                ),
            })?;
            package_name_from_lakefile(project_root, target_name, &lakefile)
        }
        Err(err) => Err(LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not read {} ({err})", manifest_path.display()),
        }),
    }
}

fn normalize_lake_identifier(raw: &str) -> String {
    raw.trim()
        .trim_matches('«')
        .trim_matches('»')
        .trim_matches('"')
        .trim()
        .to_owned()
}

struct SourceSet {
    paths: Vec<PathBuf>,
    max_mtime_ns: u128,
}

fn scan_source_set(project_root: &Path, target_name: &str) -> Result<SourceSet, LinkDiagnostics> {
    let mut paths = Vec::new();
    collect_lean_sources(project_root, project_root, target_name, &mut paths)?;
    for file_name in ["lakefile.lean", "lakefile.toml", "lean-toolchain"] {
        let path = project_root.join(file_name);
        if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();

    let mut max_mtime_ns = 0;
    for path in &paths {
        let metadata = fs::metadata(path).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not stat {} ({err})", path.display()),
        })?;
        let modified = metadata
            .modified()
            .map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
                project_root: project_root.to_path_buf(),
                target_name: target_name.to_owned(),
                reason: format!("could not read mtime for {} ({err})", path.display()),
            })?;
        let mtime_ns = modified
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        max_mtime_ns = max_mtime_ns.max(mtime_ns);
    }

    Ok(SourceSet { paths, max_mtime_ns })
}

fn collect_lean_sources(
    project_root: &Path,
    dir: &Path,
    target_name: &str,
    paths: &mut Vec<PathBuf>,
) -> Result<(), LinkDiagnostics> {
    for entry in fs::read_dir(dir).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not read directory {} ({err})", dir.display()),
    })? {
        let entry = entry.map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not read directory entry under {} ({err})", dir.display()),
        })?;
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == ".lake") {
            continue;
        }
        let metadata = entry.metadata().map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not stat {} ({err})", path.display()),
        })?;
        if metadata.is_dir() {
            collect_lean_sources(project_root, &path, target_name, paths)?;
        } else if path.extension().is_some_and(|ext| ext == "lean") {
            paths.push(path);
        }
    }
    Ok(())
}

const GENERATED_OUTPUT_PARENT: &str = "lean-rs-generated";

fn validate_generated_output_key(
    project_root: &Path,
    target_name: &str,
    key: Option<&str>,
) -> Result<(), LinkDiagnostics> {
    let Some(key) = key else { return Ok(()) };
    if key.is_empty()
        || key.len() > 64
        || matches!(key, "." | "..")
        || key.starts_with('.')
        || key.ends_with('.')
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: "generated output key must be one lowercase ASCII component of at most 64 bytes without edge dots"
                .to_owned(),
        });
    }
    validate_generated_output_directories(project_root, target_name, key)?;
    Ok(())
}

fn validate_generated_output_directories(
    project_root: &Path,
    target_name: &str,
    key: &str,
) -> Result<(), LinkDiagnostics> {
    let selected = project_root.join(generated_build_dir_relative(key));
    for path in [
        project_root.join(".lake"),
        project_root.join(".lake").join(GENERATED_OUTPUT_PARENT),
        selected.clone(),
        selected.join("lib"),
        selected.join("lean-rs-build-cache"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && path.canonicalize().is_ok_and(|actual| actual == path) => {}
            Ok(_) => {
                return Err(LinkDiagnostics::LakeOutputUnresolved {
                    project_root: project_root.to_path_buf(),
                    target_name: target_name.to_owned(),
                    reason: format!(
                        "generated output directory is aliased or not a directory: {}",
                        path.display()
                    ),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(input_diagnostic(project_root, target_name, &error)),
        }
    }
    Ok(())
}

fn validate_generated_output_file(project_root: &Path, target_name: &str, path: &Path) -> Result<(), LinkDiagnostics> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && path.canonicalize().is_ok_and(|actual| actual == path)
                && generated_file_has_one_link(&metadata) =>
        {
            Ok(())
        }
        Ok(_) => Err(LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!(
                "generated output file is aliased or not a regular file: {}",
                path.display()
            ),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(input_diagnostic(project_root, target_name, &error)),
    }
}

#[cfg(unix)]
fn generated_file_has_one_link(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    metadata.nlink() == 1
}

#[cfg(not(unix))]
fn generated_file_has_one_link(_metadata: &fs::Metadata) -> bool {
    true
}

fn generated_build_dir_relative(key: &str) -> PathBuf {
    Path::new(".lake").join(GENERATED_OUTPUT_PARENT).join(key)
}

fn selected_build_dir(project_root: &Path, options: &LakeBuildOptions) -> PathBuf {
    options.generated_output_key.as_deref().map_or_else(
        || project_root.join(".lake/build"),
        |key| project_root.join(generated_build_dir_relative(key)),
    )
}

fn resolve_dylib_path(
    project_root: &Path,
    package_name: &str,
    target_name: &str,
    options: &LakeBuildOptions,
) -> PathBuf {
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = selected_build_dir(project_root, options).join("lib");
    let escaped_package = package_name.replace('_', "__");
    let new_style = lib_dir.join(format!("lib{escaped_package}_{target_name}.{dylib_extension}"));
    let old_style = lib_dir.join(format!("lib{target_name}.{dylib_extension}"));
    if new_style.is_file() {
        new_style
    } else if old_style.is_file() {
        old_style
    } else {
        new_style
    }
}

fn cache_path(project_root: &Path, target_name: &str, options: &LakeBuildOptions) -> PathBuf {
    let root = options.generated_output_key.as_ref().map_or_else(
        || project_root.join(".lake/lean-rs-build-cache"),
        |_| selected_build_dir(project_root, options).join("lean-rs-build-cache"),
    );
    root.join(format!("{}.cache", sanitize_target_name(target_name)))
}

fn write_capability_manifest(
    project_root: &Path,
    target_name: &str,
    package: &str,
    module: &str,
    dylib_path: &Path,
    manifest_env_var: &str,
    export_signatures: &[LeanExportSignature],
    explicit_dependencies: &[LeanLibraryDependency],
    selected_toolchain: Option<&ToolchainInfo>,
    options: &LakeBuildOptions,
) -> Result<PathBuf, LinkDiagnostics> {
    let manifest_path = capability_manifest_path(project_root, target_name, export_signatures, options);
    if options.generated_output_key.is_some() {
        validate_generated_output_file(project_root, target_name, &manifest_path)?;
    }
    let manifest = capability_manifest_value(
        project_root,
        target_name,
        package,
        module,
        dylib_path,
        manifest_env_var,
        export_signatures,
        explicit_dependencies,
        selected_toolchain,
        options,
    )?;
    check_manifest_dependency_files(&manifest).map_err(|error| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: error.to_string(),
    })?;
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not encode Lean capability manifest ({err})"),
    })?;
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not create manifest directory {} ({err})", parent.display()),
        })?;
    }
    write_atomic(&manifest_path, &bytes).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!(
            "could not atomically write Lean capability manifest {} ({err})",
            manifest_path.display()
        ),
    })?;
    Ok(manifest_path)
}

// Check the same ordered dependency list on the read-only probe and immediately
// before writer publication. Lake can produce some of these files during the build.
fn check_manifest_dependency_files(manifest: &serde_json::Value) -> io::Result<()> {
    let dependencies = manifest["dependencies"]
        .as_array()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing manifest dependency list"))?;
    for dependency in dependencies {
        let path = dependency["dylib_path"]
            .as_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing dependency dylib path"))?;
        let metadata = fs::metadata(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not inspect dependency dylib {path}: {error}"),
            )
        })?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("expected dependency dylib file at {path}"),
            ));
        }
    }
    Ok(())
}

fn capability_manifest_value(
    project_root: &Path,
    target_name: &str,
    package: &str,
    module: &str,
    dylib_path: &Path,
    manifest_env_var: &str,
    export_signatures: &[LeanExportSignature],
    explicit_dependencies: &[LeanLibraryDependency],
    selected_toolchain: Option<&ToolchainInfo>,
    options: &LakeBuildOptions,
) -> Result<serde_json::Value, LinkDiagnostics> {
    let mut dependencies = capability_dependencies(project_root, target_name, options)?;
    dependencies.extend(explicit_dependencies.iter().map(lean_library_dependency_to_json));
    let fingerprint = manifest_toolchain_fingerprint(project_root, target_name, selected_toolchain)?;
    let search_dirs = capability_search_dirs(project_root, dylib_path, options);
    let build_toolchain = selected_toolchain.map(|info| {
        serde_json::json!({
            "source": format!("{:?}", info.source),
            "sysroot": info.prefix.display().to_string(),
            "version": &info.version,
            "lean_binary": info.lean_binary.as_ref().map(|path| path.display().to_string()),
        })
    });
    Ok(serde_json::json!({
        "schema_version": CAPABILITY_MANIFEST_SCHEMA_VERSION,
        "target_name": target_name,
        "package": package,
        "module": module,
        "primary_dylib": dylib_path.display().to_string(),
        "manifest_env_var": manifest_env_var,
        "lean_version": &fingerprint.lean_version,
        "resolved_lean_version": &fingerprint.resolved_version,
        "lean_header_sha256": &fingerprint.header_sha256,
        "toolchain_fingerprint": {
            "lean_version": &fingerprint.lean_version,
            "resolved_version": &fingerprint.resolved_version,
            "header_sha256": &fingerprint.header_sha256,
            "fixture_sha256": fingerprint.fixture_sha256,
            "host_triple": fingerprint.host_triple,
        },
        "build_toolchain": build_toolchain,
        "search_dirs": search_dirs
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "dependencies": dependencies,
        "exports": export_signatures
            .iter()
            .map(LeanExportSignature::to_json)
            .collect::<Vec<_>>(),
    }))
}

struct ManifestToolchainFingerprint {
    lean_version: String,
    resolved_version: String,
    header_sha256: String,
    fixture_sha256: &'static str,
    host_triple: &'static str,
}

fn manifest_toolchain_fingerprint(
    project_root: &Path,
    target_name: &str,
    selected_toolchain: Option<&ToolchainInfo>,
) -> Result<ManifestToolchainFingerprint, LinkDiagnostics> {
    let Some(info) = selected_toolchain else {
        let current = ToolchainFingerprint::current();
        return Ok(ManifestToolchainFingerprint {
            lean_version: current.lean_version.to_owned(),
            resolved_version: current.resolved_version.to_owned(),
            header_sha256: current.header_sha256.to_owned(),
            fixture_sha256: current.fixture_sha256,
            host_triple: current.host_triple,
        });
    };
    let header_sha256 = sha256_manifest_input(project_root, target_name, &info.header_path)?;
    let Some(entry) = lean_rs_abi::supported_by_digest(&header_sha256) else {
        return Err(LinkDiagnostics::UnsupportedToolchain {
            active: info.version.clone(),
            supported_window: supported_window(),
        });
    };
    let resolved_version = if entry.versions.contains(&info.version.as_str()) {
        info.version.clone()
    } else {
        entry.versions.first().copied().unwrap_or("unknown").to_owned()
    };
    Ok(ManifestToolchainFingerprint {
        lean_version: info.version.clone(),
        resolved_version,
        header_sha256,
        fixture_sha256: ToolchainFingerprint::current().fixture_sha256,
        host_triple: ToolchainFingerprint::current().host_triple,
    })
}

fn sha256_manifest_input(project_root: &Path, target_name: &str, path: &Path) -> Result<String, LinkDiagnostics> {
    let bytes = fs::read(path).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not read selected Lean header {} ({err})", path.display()),
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn supported_window() -> String {
    lean_rs_abi::SUPPORTED_TOOLCHAINS
        .iter()
        .map(|toolchain| format!("{:?}", toolchain.versions))
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("manifest");
    for attempt in 0..100_u32 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp_path = parent.join(format!(".{file_name}.{}.{}.{}.tmp", std::process::id(), nanos, attempt));
        match OpenOptions::new().write(true).create_new(true).open(&tmp_path) {
            Ok(mut file) => {
                if let Err(err) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    drop(file);
                    drop(fs::remove_file(&tmp_path));
                    return Err(err);
                }
                drop(file);
                if let Err(err) = fs::rename(&tmp_path, path) {
                    drop(fs::remove_file(&tmp_path));
                    return Err(err);
                }
                return Ok(());
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("could not allocate temporary path for {}", path.display()),
    ))
}

fn capability_manifest_path(
    project_root: &Path,
    target_name: &str,
    export_signatures: &[LeanExportSignature],
    options: &LakeBuildOptions,
) -> PathBuf {
    let manifest_name = capability_manifest_name(target_name, export_signatures);
    if options.generated_output_key.is_some() {
        selected_build_dir(project_root, options)
            .join("lean-rs-build-cache")
            .join(manifest_name)
    } else if let Some(out_dir) = env::var_os("OUT_DIR") {
        PathBuf::from(out_dir).join(manifest_name)
    } else {
        project_root
            .join(".lake")
            .join("lean-rs-build-cache")
            .join(manifest_name)
    }
}

fn capability_manifest_name(target_name: &str, export_signatures: &[LeanExportSignature]) -> String {
    let target = sanitize_target_name(target_name);
    if export_signatures.is_empty() {
        return format!("{target}.lean-rs-capability.json");
    }
    let mut hasher = Sha256::new();
    for signature in export_signatures {
        hasher.update(signature.symbol().as_bytes());
        hasher.update([0]);
        if let Ok(bytes) = serde_json::to_vec(&signature.to_json()) {
            hasher.update(bytes);
        }
        hasher.update([0xff]);
    }
    let digest = hasher.finalize();
    let mut suffix = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        let _ = write!(&mut suffix, "{byte:02x}");
    }
    format!("{target}-{suffix}.lean-rs-capability.json")
}

fn capability_search_dirs(project_root: &Path, dylib_path: &Path, options: &LakeBuildOptions) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(parent) = dylib_path.parent() {
        dirs.push(parent.to_path_buf());
    }
    dirs.push(selected_build_dir(project_root, options).join("lib"));
    dirs.sort();
    dirs.dedup();
    dirs
}

fn capability_dependencies(
    project_root: &Path,
    target_name: &str,
    options: &LakeBuildOptions,
) -> Result<Vec<serde_json::Value>, LinkDiagnostics> {
    let manifest_path = project_root.join("lake-manifest.json");
    let bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(LinkDiagnostics::LakeOutputUnresolved {
                project_root: project_root.to_path_buf(),
                target_name: target_name.to_owned(),
                reason: format!("could not read {} ({err})", manifest_path.display()),
            });
        }
    };
    let manifest: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("{} is not valid JSON ({err})", manifest_path.display()),
        })?;
    let packages = manifest
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .map_or([].as_slice(), Vec::as_slice);
    let mut dependencies = Vec::new();
    for package in packages {
        let Some(name) = package.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if name != "lean_rs_interop_shims" {
            continue;
        }
        let Some(dir) = package.get("dir").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let dependency_root = project_root.join(dir);
        let dependency_root =
            fs::canonicalize(&dependency_root).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
                project_root: project_root.to_path_buf(),
                target_name: target_name.to_owned(),
                reason: format!(
                    "could not canonicalize dependency root {} ({err})",
                    dependency_root.display()
                ),
            })?;
        let dylib = resolve_dylib_path(&dependency_root, "lean_rs_interop_shims", "LeanRsInterop", options);
        if let Some(key) = options.generated_output_key.as_deref() {
            validate_generated_output_directories(&dependency_root, target_name, key)?;
            validate_generated_output_file(&dependency_root, target_name, &dylib)?;
        }
        dependencies.push(serde_json::json!({
            "name": name,
            "dylib_path": dylib.display().to_string(),
            "export_symbols_for_dependents": true,
            "initializer": {
                "package": "lean_rs_interop_shims",
                "module": "LeanRsInterop",
            }
        }));
    }
    Ok(dependencies)
}

fn lean_library_dependency_to_json(dependency: &LeanLibraryDependency) -> serde_json::Value {
    let initializer = dependency.module_initializer().map(|initializer| {
        serde_json::json!({
            "package": initializer.package_name(),
            "module": initializer.module_name(),
        })
    });
    let name = dependency.module_initializer().map_or_else(
        || stable_dependency_name(dependency.path_ref()),
        |initializer| initializer.package_name().to_owned(),
    );
    serde_json::json!({
        "name": name,
        "dylib_path": dependency.path_ref().display().to_string(),
        "export_symbols_for_dependents": dependency.exports_symbols_for_dependents(),
        "initializer": initializer,
    })
}

fn stable_dependency_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("lean_dependency")
        .to_owned()
}

fn sanitize_target_name(target_name: &str) -> String {
    target_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn screaming_snake(input: &str) -> String {
    let mut out = String::new();
    let mut prev_was_sep = true;
    let mut prev_was_lower_or_digit = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            if ch.is_ascii_uppercase() && prev_was_lower_or_digit && !prev_was_sep {
                out.push('_');
            }
            out.push(ch.to_ascii_uppercase());
            prev_was_sep = false;
            prev_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            if !prev_was_sep {
                out.push('_');
            }
            prev_was_sep = true;
            prev_was_lower_or_digit = false;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() { "CAPABILITY".to_owned() } else { out }
}

fn file_observation(metadata: &fs::Metadata) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend(metadata.len().to_le_bytes());
    bytes.extend(
        metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos()
            .to_le_bytes(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        bytes.extend(metadata.dev().to_le_bytes());
        bytes.extend(metadata.ino().to_le_bytes());
        bytes.extend(metadata.ctime().to_le_bytes());
        bytes.extend(metadata.ctime_nsec().to_le_bytes());
    }
    Ok(bytes)
}

fn content_digest(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let before = file_observation(&file.metadata()?)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hasher.update(chunk.get(..read).ok_or_else(|| io::Error::other("invalid read size"))?);
    }
    if before != file_observation(&file.metadata()?)? || before != file_observation(&fs::metadata(path)?)? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "build input changed while being fingerprinted",
        ));
    }
    Ok(hasher.finalize().into())
}

fn hash_path(hasher: &mut Sha256, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    hasher.update(bytes.len().to_le_bytes());
    hasher.update(bytes);
}

fn input_diagnostic(root: &Path, target: &str, error: &io::Error) -> LinkDiagnostics {
    LinkDiagnostics::LakeOutputUnresolved {
        project_root: root.to_path_buf(),
        target_name: target.to_owned(),
        reason: format!("could not fingerprint Lake build inputs ({error})"),
    }
}

fn collect_build_inputs(root: &Path, paths: &mut Vec<PathBuf>, directories: &mut Vec<PathBuf>) -> io::Result<()> {
    directories.push(root.to_path_buf());
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if matches!(entry.file_name().to_str(), Some(".lake" | ".git")) {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            if !fs::metadata(&path)?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source aliases must resolve to regular files; directory aliases require a declared canonical workspace root",
                ));
            }
            paths.push(path);
        } else if kind.is_dir() {
            collect_build_inputs(&path, paths, directories)?;
        } else if kind.is_file() {
            paths.push(path);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Lake source inputs must be regular files",
            ));
        }
    }
    Ok(())
}

fn build_input_roots(root: &Path, target: &str) -> Result<Vec<PathBuf>, LinkDiagnostics> {
    Ok(
        if root
            .join("lake-manifest.json")
            .try_exists()
            .map_err(|e| input_diagnostic(root, target, &e))?
        {
            crate::shared_writer::project_artifact_roots(root).map_err(|e| input_diagnostic(root, target, &e))?
        } else {
            vec![root.to_path_buf()]
        },
    )
}

fn build_inputs_fingerprint(root: &Path, target: &str) -> Result<String, LinkDiagnostics> {
    // The direct build-script helper historically lets Lake create a missing
    // manifest. Runtime writer admission separately requires one; if creation
    // changes this input set during a build, publication is refused below.
    let roots = build_input_roots(root, target)?;
    let mut hasher = Sha256::new();
    hasher.update(b"lean-rs-content-inputs-v2");
    for dependency in roots {
        hash_path(&mut hasher, &dependency);
        let mut paths = Vec::new();
        let mut directories = Vec::new();
        collect_build_inputs(&dependency, &mut paths, &mut directories)
            .map_err(|e| input_diagnostic(root, target, &e))?;
        directories.sort();
        let observations = directories
            .into_iter()
            .map(|path| {
                let observed = file_observation(&fs::metadata(&path)?)?;
                Ok((path, observed))
            })
            .collect::<io::Result<Vec<_>>>()
            .map_err(|e| input_diagnostic(root, target, &e))?;
        for name in ["lake-manifest.json", ".lake/package-overrides.json"] {
            let path = dependency.join(name);
            if path.try_exists().map_err(|e| input_diagnostic(root, target, &e))? {
                paths.push(path);
            }
        }
        paths.sort();
        paths.dedup();
        for path in paths {
            hash_path(
                &mut hasher,
                path.strip_prefix(&dependency)
                    .map_err(|e| input_diagnostic(root, target, &io::Error::other(e)))?,
            );
            let before = file_observation(&fs::metadata(&path).map_err(|e| input_diagnostic(root, target, &e))?)
                .map_err(|e| input_diagnostic(root, target, &e))?;
            hasher.update(content_digest(&path).map_err(|e| input_diagnostic(root, target, &e))?);
            let after = file_observation(&fs::metadata(&path).map_err(|e| input_diagnostic(root, target, &e))?)
                .map_err(|e| input_diagnostic(root, target, &e))?;
            if before != after {
                return Err(input_diagnostic(
                    root,
                    target,
                    &io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "build source changed while being fingerprinted",
                    ),
                ));
            }
            hasher.update(after);
        }
        for (path, observed) in observations {
            if file_observation(&fs::metadata(&path).map_err(|e| input_diagnostic(root, target, &e))?)
                .map_err(|e| input_diagnostic(root, target, &e))?
                != observed
            {
                return Err(input_diagnostic(
                    root,
                    target,
                    &io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "source directory changed while being fingerprinted",
                    ),
                ));
            }
            hash_path(
                &mut hasher,
                path.strip_prefix(&dependency)
                    .map_err(|e| input_diagnostic(root, target, &io::Error::other(e)))?,
            );
            hasher.update(observed);
        }
        hasher.update([0xff]);
    }
    Ok(hex(&hasher.finalize()))
}

fn dependency_artifact_fingerprint(
    root: &Path,
    target: &str,
    options: &LakeBuildOptions,
) -> Result<String, LinkDiagnostics> {
    let mut dependencies = capability_dependencies(root, target, options)?;
    dependencies.extend(options.dependencies.iter().map(lean_library_dependency_to_json));
    let mut paths = std::collections::BTreeSet::new();
    for dependency in dependencies {
        if let Some(path) = dependency.get("dylib_path").and_then(serde_json::Value::as_str) {
            paths.insert(PathBuf::from(path));
        }
    }
    let mut hasher = Sha256::new();
    for path in paths {
        hash_path(&mut hasher, &path);
        match content_digest(&path) {
            Ok(digest) => {
                hasher.update([1]);
                hasher.update(digest);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => hasher.update([0]),
            Err(error) => return Err(input_diagnostic(root, target, &error)),
        }
    }
    Ok(hex(&hasher.finalize()))
}

fn cache_key(
    target_name: &str,
    package_name: &str,
    manifest_digest: &str,
    source_set: &SourceSet,
    options: &LakeBuildOptions,
    inputs: &str,
    dependency_artifacts: &str,
) -> String {
    let sysroot = options
        .lean_sysroot
        .as_deref()
        .map_or("ambient", |path| path.to_str().unwrap_or("<non-utf8-sysroot>"));
    let mut key = format!(
        "cache_version=2\ntarget={target_name}\npackage={package_name}\nmanifest={manifest_digest}\nsource_count={}\nsource_max_mtime_ns={}\nlean_sysroot={sysroot}\n",
        source_set.paths.len(),
        source_set.max_mtime_ns
    );
    if let Some(generated_output) = &options.generated_output_key {
        let _ = writeln!(key, "generated_output={generated_output}");
    }
    let _ = write!(
        key,
        "inputs_sha256={inputs}\ndependency_artifacts_sha256={dependency_artifacts}\n"
    );
    key
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn command_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut raw = String::new();
    if !stderr.is_empty() {
        raw.push_str(&String::from_utf8_lossy(stderr));
    }
    if !stdout.is_empty() {
        if !raw.is_empty() {
            raw.push_str(" | ");
        }
        raw.push_str(&String::from_utf8_lossy(stdout));
    }
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "no output".to_owned()
    } else if collapsed.len() > 1024 {
        let mut bounded = collapsed.chars().take(1024).collect::<String>();
        bounded.push_str("...");
        bounded
    } else {
        collapsed
    }
}

fn emit_lake_trace(args: std::fmt::Arguments<'_>) {
    let mut stderr = io::stderr().lock();
    drop(stderr.write_fmt(args));
    drop(stderr.write_all(b"\n"));
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]
mod tests {
    use super::{
        CAPABILITY_MANIFEST_SCHEMA_VERSION, CargoLeanCapability, CargoMetadata, LakeBuildOptions, LakeRun, LakeRunner,
        build_lake_target_with_runner, build_lake_target_with_runner_and_options, capability_env_var,
        capability_manifest_env_var, capability_manifest_name, command_detail, generated_output_key_for_sysroot,
    };
    use crate::LinkDiagnostics;
    use crate::{
        LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
        LeanExportSignature, LeanLibraryDependency,
    };
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    #[test]
    fn build_selected_sysroot_uses_the_supported_version_output_policy() {
        let sysroot = Path::new(crate::LEAN_HEADER_PATH)
            .ancestors()
            .nth(3)
            .expect("build-selected header has a sysroot");
        let expected = (crate::LEAN_VERSION == "4.36.0").then_some("lean-4.36.0");
        assert_eq!(
            generated_output_key_for_sysroot(sysroot).expect("supported exact build-selected sysroot"),
            expected
        );
    }

    #[derive(Clone)]
    struct FakeLake {
        calls: Rc<Cell<usize>>,
        seen_sysroots: Rc<RefCell<Vec<Option<PathBuf>>>>,
        seen_threads: Rc<RefCell<Vec<Option<u32>>>>,
        seen_generated_outputs: Rc<RefCell<Vec<Option<String>>>>,
        mode: FakeMode,
    }

    #[derive(Clone)]
    enum FakeMode {
        SuccessModern,
        SuccessLegacy,
        Failure,
        SpawnError,
    }

    impl FakeLake {
        fn new(mode: FakeMode) -> Self {
            Self {
                calls: Rc::new(Cell::new(0)),
                seen_sysroots: Rc::new(RefCell::new(Vec::new())),
                seen_threads: Rc::new(RefCell::new(Vec::new())),
                seen_generated_outputs: Rc::new(RefCell::new(Vec::new())),
                mode,
            }
        }

        fn calls(&self) -> usize {
            self.calls.get()
        }

        fn seen_sysroots(&self) -> Vec<Option<PathBuf>> {
            self.seen_sysroots.borrow().clone()
        }

        fn seen_threads(&self) -> Vec<Option<u32>> {
            self.seen_threads.borrow().clone()
        }

        fn seen_generated_outputs(&self) -> Vec<Option<String>> {
            self.seen_generated_outputs.borrow().clone()
        }
    }

    impl LakeRunner for FakeLake {
        fn build_shared(
            &mut self,
            project_root: &Path,
            target_name: &str,
            options: &LakeBuildOptions,
        ) -> Result<LakeRun, std::io::Error> {
            self.calls.set(self.calls.get().saturating_add(1));
            self.seen_sysroots.borrow_mut().push(options.lean_sysroot.clone());
            self.seen_threads.borrow_mut().push(options.lean_num_threads);
            self.seen_generated_outputs
                .borrow_mut()
                .push(options.generated_output_key.clone());
            // Model cold Lake dependency output creation. Existing dependency
            // cache regressions must survive this excluded-output mutation.
            if project_root.join("lake-manifest.json").is_file() {
                for root in super::build_input_roots(project_root, target_name).map_err(std::io::Error::other)? {
                    fs::create_dir_all(root.join(".lake"))?;
                }
            }
            match self.mode {
                FakeMode::SuccessModern => {
                    let dylib = super::selected_build_dir(project_root, options)
                        .join("lib")
                        .join(format!("libmy__pkg_{target_name}.{}", dylib_ext()));
                    write_file(&dylib, "dylib");
                    Ok(success_run())
                }
                FakeMode::SuccessLegacy => {
                    let dylib = super::selected_build_dir(project_root, options)
                        .join("lib")
                        .join(format!("lib{target_name}.{}", dylib_ext()));
                    write_file(&dylib, "dylib");
                    Ok(success_run())
                }
                FakeMode::Failure => Ok(LakeRun {
                    success: false,
                    status: "exit status: 1".to_owned(),
                    stdout: b"stdout detail\n".to_vec(),
                    stderr: b"stderr detail\n".to_vec(),
                }),
                FakeMode::SpawnError => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "lake missing")),
            }
        }
    }

    fn success_run() -> LakeRun {
        LakeRun {
            success: true,
            status: "exit status: 0".to_owned(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn dylib_ext() -> &'static str {
        if cfg!(target_os = "macos") { "dylib" } else { "so" }
    }

    fn make_project(name: &str, target: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lean-toolchain-lake-{}-{}", std::process::id(), name));
        drop(fs::remove_dir_all(&root));
        populate_project(&root, target);
        root
    }

    struct GeneratedOutputFixture(PathBuf);
    impl Drop for GeneratedOutputFixture {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.0));
        }
    }

    fn make_generated_output_project(name: &str, target: &str) -> GeneratedOutputFixture {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("workspace root");
        let root = workspace
            .join("target/test-state")
            .join(format!("generated-output-{}-{name}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        populate_project(&root, target);
        GeneratedOutputFixture(root)
    }

    fn populate_project(root: &Path, target: &str) {
        fs::create_dir_all(root).expect("create temp project");
        write_file(
            &root.join("lakefile.lean"),
            &format!(
                "import Lake\nopen Lake DSL\npackage «my_pkg»\n@[default_target]\nlean_lib «{target}» where\n  defaultFacets := #[LeanLib.sharedFacet]\n"
            ),
        );
        write_file(
            &root.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[],"name":"my_pkg","lakeDir":".lake"}"#,
        );
        write_file(&root.join("lean-toolchain"), "leanprover/lean4:v4.29.1\n");
        write_file(&root.join(format!("{target}.lean")), "def hello : Nat := 1\n");
    }

    fn make_toml_project(name: &str, target: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lean-toolchain-lake-{}-{}", std::process::id(), name));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).expect("create temp project");
        write_file(
            &root.join("lakefile.toml"),
            &format!("name = \"my_pkg\"\ndefaultTargets = [\"{target}\"]\n\n[[lean_lib]]\nname = \"{target}\"\n"),
        );
        write_file(
            &root.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[],"name":"my_pkg","lakeDir":".lake"}"#,
        );
        write_file(&root.join("lean-toolchain"), "leanprover/lean4:v4.29.1\n");
        write_file(&root.join(format!("{target}.lean")), "def hello : Nat := 1\n");
        root
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    fn make_fake_sysroot(name: &str) -> PathBuf {
        make_fake_sysroot_with_version(name, crate::LEAN_VERSION)
    }

    fn make_fake_sysroot_with_version(name: &str, version: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lean-toolchain-sysroot-{}-{name}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        let include = root.join("include").join("lean");
        fs::create_dir_all(&include).expect("create fake include dir");
        fs::copy(crate::LEAN_HEADER_PATH, include.join("lean.h")).expect("copy supported lean.h");
        write_file(
            &include.join("version.h"),
            &format!("#define LEAN_VERSION_STRING \"{version}\"\n"),
        );
        root
    }

    fn header_identical_alias() -> Option<&'static str> {
        match crate::LEAN_VERSION {
            "4.31.0-rc1" => Some("4.31.0-rc2"),
            "4.31.0-rc2" => Some("4.31.0-rc1"),
            _ => None,
        }
    }

    #[test]
    fn cache_hit_skips_lake_invocation() {
        let root = make_project("cache-hit", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let first = build_lake_target_with_runner(&root, "MyCapability", &mut runner, CargoMetadata::Emit)
            .expect("first build");
        let second = build_lake_target_with_runner(&root, "MyCapability", &mut runner, CargoMetadata::Emit)
            .expect("cached build");

        assert_eq!(first, second);
        assert_eq!(runner.calls(), 1, "second call should use cache");
    }

    #[test]
    fn explicit_lake_sysroot_is_part_of_runner_options_and_cache_key() {
        let root = make_project("explicit-sysroot-cache", "MyCapability");
        let sysroot = PathBuf::from("/configured/lean/sysroot");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let options = LakeBuildOptions {
            lean_sysroot: Some(sysroot.clone()),
            ..LakeBuildOptions::default()
        };
        let first = build_lake_target_with_runner_and_options(
            &root,
            "MyCapability",
            &mut runner,
            CargoMetadata::Emit,
            &options,
        )
        .expect("first explicit build");
        let second = build_lake_target_with_runner_and_options(
            &root,
            "MyCapability",
            &mut runner,
            CargoMetadata::Emit,
            &options,
        )
        .expect("cached explicit build");

        assert_eq!(first, second);
        assert_eq!(runner.calls(), 1, "second call should use explicit-sysroot cache");
        assert_eq!(runner.seen_sysroots(), vec![Some(sysroot)]);
    }

    #[test]
    fn missing_manifest_lets_lake_create_manifest() {
        let root = make_project("missing-manifest", "MyCapability");
        fs::remove_file(root.join("lake-manifest.json")).expect("remove manifest");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let path = build_lake_target_with_runner(&root, "MyCapability", &mut runner, CargoMetadata::Emit)
            .expect("build without checked-in manifest");

        assert!(path.ends_with(format!("libmy__pkg_MyCapability.{}", dylib_ext())));
        assert_eq!(runner.calls(), 1);
    }

    #[test]
    fn legacy_output_path_is_supported() {
        let root = make_project("legacy", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessLegacy);
        let path = build_lake_target_with_runner(&root, "MyCapability", &mut runner, CargoMetadata::Emit)
            .expect("legacy build");

        assert!(path.ends_with(format!("libMyCapability.{}", dylib_ext())));
    }

    #[test]
    fn missing_target_is_typed() {
        let root = make_project("missing-target", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let err = build_lake_target_with_runner(&root, "OtherTarget", &mut runner, CargoMetadata::Emit)
            .expect_err("missing target");

        match err {
            LinkDiagnostics::LakeTargetMissing { target_name, .. } => assert_eq!(target_name, "OtherTarget"),
            other => panic!("expected LakeTargetMissing, got {other:?}"),
        }
        assert_eq!(runner.calls(), 0);
    }

    #[test]
    fn toml_lakefile_build_succeeds() {
        let root = make_toml_project("toml-success", "FixtureLib");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let path = build_lake_target_with_runner(&root, "FixtureLib", &mut runner, CargoMetadata::Emit)
            .expect("TOML lakefile build");

        assert!(path.ends_with(format!("libmy__pkg_FixtureLib.{}", dylib_ext())));
        assert_eq!(runner.calls(), 1);
    }

    #[test]
    fn toml_lakefile_missing_target_is_typed() {
        let root = make_toml_project("toml-missing", "FixtureLib");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let err = build_lake_target_with_runner(&root, "OtherTarget", &mut runner, CargoMetadata::Emit)
            .expect_err("missing TOML target");

        match err {
            LinkDiagnostics::LakeTargetMissing { target_name, .. } => assert_eq!(target_name, "OtherTarget"),
            other => panic!("expected LakeTargetMissing, got {other:?}"),
        }
        assert_eq!(runner.calls(), 0);
    }

    #[test]
    fn toml_lakefile_missing_manifest_resolves_package() {
        let root = make_toml_project("toml-no-manifest", "FixtureLib");
        fs::remove_file(root.join("lake-manifest.json")).expect("remove manifest");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let path = build_lake_target_with_runner(&root, "FixtureLib", &mut runner, CargoMetadata::Emit)
            .expect("TOML build without manifest");

        assert!(path.ends_with(format!("libmy__pkg_FixtureLib.{}", dylib_ext())));
        assert_eq!(runner.calls(), 1);
    }

    #[test]
    fn build_failure_is_typed_and_one_line() {
        let root = make_project("failure", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::Failure);
        let err = build_lake_target_with_runner(&root, "MyCapability", &mut runner, CargoMetadata::Emit)
            .expect_err("failure");
        let rendered = format!("{err}");

        match err {
            LinkDiagnostics::LakeBuildFailed { detail, .. } => {
                assert!(detail.contains("stderr detail"));
                assert!(detail.contains("stdout detail"));
                assert!(!detail.contains('\n'));
            }
            other => panic!("expected LakeBuildFailed, got {other:?}"),
        }
        assert!(!rendered.contains('\n'));
    }

    #[test]
    fn missing_lake_is_typed() {
        let root = make_project("spawn-error", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SpawnError);
        let err = build_lake_target_with_runner(&root, "MyCapability", &mut runner, CargoMetadata::Emit)
            .expect_err("spawn error");

        match err {
            LinkDiagnostics::LakeUnavailable {
                target_name, detail, ..
            } => {
                assert_eq!(target_name, "MyCapability");
                assert!(detail.contains("lake missing"));
            }
            other => panic!("expected LakeUnavailable, got {other:?}"),
        }
        assert_eq!(runner.calls(), 1);
    }

    #[test]
    fn cache_hit_skips_lake_invocation_for_interop_dependency_shape() {
        let root = make_project("interop-cache-hit", "InteropConsumer");
        let dependency = root.join("interop");
        write_file(&dependency.join("lake-manifest.json"), r#"{"packages":[]}"#);
        write_file(&dependency.join("Interop.lean"), "def interop := 1");
        write_file(
            &root.join("lakefile.lean"),
            "import Lake\nopen Lake DSL\npackage «my_pkg»\nrequire «lean_rs_interop_shims» from \"../../crates/lean-rs/shims/lean-rs-interop-shims\"\n@[default_target]\nlean_lib «InteropConsumer» where\n  defaultFacets := #[LeanLib.sharedFacet]\n",
        );
        write_file(
            &root.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[{"type":"path","scope":"","name":"lean_rs_interop_shims","manifestFile":"lake-manifest.json","inherited":false,"dir":"interop","configFile":"lakefile.lean"}],"name":"my_pkg","lakeDir":".lake"}"#,
        );
        let mut runner = FakeLake::new(FakeMode::SuccessModern);

        let first = build_lake_target_with_runner(&root, "InteropConsumer", &mut runner, CargoMetadata::Emit)
            .expect("first build");
        let second = build_lake_target_with_runner(&root, "InteropConsumer", &mut runner, CargoMetadata::Emit)
            .expect("cached build");

        assert_eq!(first, second);
        assert_eq!(runner.calls(), 1, "second call should use cache");
    }

    #[test]
    fn command_detail_is_bounded() {
        let detail = command_detail(&vec![b'x'; 4096], b"");
        assert!(detail.len() <= 1027);
        assert!(detail.ends_with("..."));
    }

    #[test]
    fn capability_env_var_is_deterministic() {
        assert_eq!(
            capability_env_var("MyCapability"),
            "LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB"
        );
        assert_eq!(
            capability_env_var("lean-dup_index"),
            "LEAN_RS_CAPABILITY_LEAN_DUP_INDEX_DYLIB"
        );
    }

    #[test]
    fn capability_manifest_env_var_is_deterministic() {
        assert_eq!(
            capability_manifest_env_var("MyCapability"),
            "LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST"
        );
        assert_eq!(
            capability_manifest_env_var("lean-dup_index"),
            "LEAN_RS_CAPABILITY_LEAN_DUP_INDEX_MANIFEST"
        );
    }

    #[test]
    fn capability_manifest_name_includes_signature_digest_for_non_empty_exports() {
        let first = vec![LeanExportSignature::function(
            "my_capability_u8_identity",
            vec![LeanExportArgAbi::new(LeanExportAbiRepr::U8, LeanExportOwnership::None)],
            LeanExportReturnAbi::new(
                LeanExportAbiRepr::U8,
                LeanExportOwnership::None,
                LeanExportResultConvention::Pure,
            ),
        )];
        let second = vec![LeanExportSignature::function(
            "my_capability_u16_identity",
            vec![LeanExportArgAbi::new(LeanExportAbiRepr::U16, LeanExportOwnership::None)],
            LeanExportReturnAbi::new(
                LeanExportAbiRepr::U16,
                LeanExportOwnership::None,
                LeanExportResultConvention::Pure,
            ),
        )];

        assert_eq!(
            capability_manifest_name("MyCapability", &[]),
            "MyCapability.lean-rs-capability.json"
        );
        let first_name = capability_manifest_name("MyCapability", &first);
        let second_name = capability_manifest_name("MyCapability", &second);
        assert_ne!(first_name, second_name);
        assert!(first_name.starts_with("MyCapability-"));
        assert!(first_name.ends_with(".lean-rs-capability.json"));
        assert!(second_name.starts_with("MyCapability-"));
        assert!(second_name.ends_with(".lean-rs-capability.json"));
    }

    #[test]
    fn dependency_content_changes_invalidate_build_and_probe_with_preserved_mtime() {
        let root = make_project("dependency-content-cache", "MyCapability");
        let dependency = make_project("dependency-content-input", "Dependency");
        let callback = dependency.join("c/callback.c");
        write_file(&callback, "int callback(void) { return 1; }\n");
        write_file(
            &root.join("lake-manifest.json"),
            &serde_json::json!({
                "version":"1.3.0", "packagesDir":".lake/packages", "name":"my_pkg",
                "packages":[{"type":"path","name":"dependency","dir":dependency,"manifestFile":"lake-manifest.json"}]
            })
            .to_string(),
        );
        let builder = CargoLeanCapability::new(&root, "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        builder
            .clone()
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("first build");
        assert!(builder.probe_ready().expect("warm dependency cache").is_some());
        let source = dependency.join("Dependency.lean");
        let before = fs::metadata(&source)
            .expect("dependency metadata")
            .modified()
            .expect("mtime");
        fs::write(&source, "def hello : Nat := 2\n").expect("same-length source rewrite");
        fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .expect("source handle")
            .set_times(fs::FileTimes::new().set_modified(before))
            .expect("preserve timestamp");
        assert!(
            builder
                .probe_ready()
                .expect("content changed under old timestamp")
                .is_none()
        );
        builder
            .clone()
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("rebuild after dependency change");
        assert_eq!(runner.calls(), 2);
        assert!(builder.probe_ready().expect("new dependency content cached").is_some());
        let dependency_builder = CargoLeanCapability::new(&dependency, "Dependency");
        dependency_builder
            .clone()
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("dependency cache");
        assert!(dependency_builder.probe_ready().expect("warm dependency").is_some());
        let before = fs::metadata(&callback)
            .expect("C input metadata")
            .modified()
            .expect("C input mtime");
        fs::write(&callback, "int callback(void) { return 2; }\n").expect("same-length C input rewrite");
        fs::OpenOptions::new()
            .write(true)
            .open(&callback)
            .expect("C input handle")
            .set_times(fs::FileTimes::new().set_modified(before))
            .expect("preserve C timestamp");
        assert!(
            dependency_builder
                .probe_ready()
                .expect("C content invalidates B")
                .is_none()
        );
        assert!(builder.probe_ready().expect("C content invalidates A").is_none());
    }

    #[test]
    fn source_change_during_build_refuses_cache_and_manifest_publication() {
        struct MutatingLake {
            lake: FakeLake,
            mutation: u8,
        }
        impl super::LakeRunner for MutatingLake {
            fn build_shared(
                &mut self,
                root: &Path,
                target: &str,
                options: &LakeBuildOptions,
            ) -> Result<super::LakeRun, std::io::Error> {
                let source = root.join(format!("{target}.lean"));
                let old = fs::read(&source)?;
                let modified = fs::metadata(&source)?.modified()?;
                let result = self.lake.build_shared(root, target, options)?;
                if self.mutation == 2 {
                    let transient = root.join("Transient.lean");
                    fs::write(&transient, "def transientInput := 1")?;
                    fs::remove_file(&transient)?;
                } else {
                    fs::write(&source, "def changedWhileBuilding := 1")?;
                    if self.mutation == 1 {
                        fs::write(&source, old)?;
                        fs::OpenOptions::new()
                            .write(true)
                            .open(&source)?
                            .set_times(fs::FileTimes::new().set_modified(modified))?;
                    }
                }
                Ok(result)
            }
        }
        for (case, target, mutation) in [
            ("changed", "MidBuildCapability", 0),
            ("restored", "RestoredMidBuildCapability", u8::from(cfg!(unix))),
            ("transient", "TransientMidBuildCapability", 2),
        ] {
            let root = make_project(&format!("inputs-{case}-mid-build"), target);
            let error = CargoLeanCapability::new(&root, target)
                .build_with_runner(
                    &mut MutatingLake {
                        lake: FakeLake::new(FakeMode::SuccessModern),
                        mutation,
                    },
                    CargoMetadata::Suppress,
                )
                .expect_err("changed input is not an attestable build");
            assert!(
                matches!(error, LinkDiagnostics::LakeOutputUnresolved { ref reason, .. } if reason.contains("inputs changed"))
            );
            assert!(!super::cache_path(&root, target, &LakeBuildOptions::default()).exists());
            assert!(!super::capability_manifest_path(&root, target, &[], &LakeBuildOptions::default()).exists());
        }
    }

    #[test]
    fn ready_probe_preserves_cache_files_and_rejects_missing_or_changed_inputs() {
        let root = make_project("ready-probe", "MyCapability");
        let builder = CargoLeanCapability::new(&root, "MyCapability").package("my_pkg");
        assert!(builder.probe_ready().expect("cold read probe").is_none());
        assert!(!super::cache_path(&root, "MyCapability", &LakeBuildOptions::default()).exists());
        let built = builder
            .clone()
            .build_with_runner(&mut FakeLake::new(FakeMode::SuccessModern), CargoMetadata::Suppress)
            .expect("fixture build and manifest publication");
        let manifest_before = fs::read(built.manifest_path()).expect("published manifest");
        let modified_before = fs::metadata(built.manifest_path())
            .expect("manifest metadata")
            .modified()
            .expect("mtime");
        assert_eq!(builder.probe_ready().expect("warm read probe"), Some(built.clone()));
        assert_eq!(
            fs::read(built.manifest_path()).expect("unchanged manifest"),
            manifest_before
        );
        assert_eq!(
            fs::metadata(built.manifest_path())
                .expect("unchanged metadata")
                .modified()
                .expect("mtime"),
            modified_before
        );
        fs::write(built.manifest_path(), b"{}").expect("corrupt public ABI metadata");
        assert!(builder.probe_ready().expect("mismatching manifest").is_none());
        assert_eq!(fs::read(built.manifest_path()).expect("not republished"), b"{}");
        fs::write(built.manifest_path(), manifest_before).expect("restore exact metadata");
        write_file(&root.join("New.lean"), "def newSource := 1");
        assert!(builder.probe_ready().expect("source set changed").is_none());
        fs::remove_file(super::cache_path(&root, "MyCapability", &LakeBuildOptions::default()))
            .expect("remove cache key");
        assert!(builder.probe_ready().expect("missing cache").is_none());
        assert!(!super::cache_path(&root, "MyCapability", &LakeBuildOptions::default()).exists());
    }

    #[test]
    fn generated_output_key_selects_only_derived_build_cache_and_manifest_paths() {
        let fixture = make_generated_output_project("selection", "MyCapability");
        let root = &fixture.0;
        let builder = CargoLeanCapability::new(root, "MyCapability")
            .package("my_pkg")
            .generated_output_key("lean-4.36.0");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let built = builder
            .clone()
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("versioned fake build");
        let selected = root
            .canonicalize()
            .expect("canonical fixture")
            .join(".lake/lean-rs-generated/lean-4.36.0");
        assert_eq!(
            super::generated_build_dir_relative("lean-4.36.0"),
            PathBuf::from(".lake/lean-rs-generated/lean-4.36.0")
        );
        assert!(built.dylib_path().starts_with(&selected));
        assert!(built.manifest_path().starts_with(&selected));
        assert_eq!(runner.seen_generated_outputs(), vec![Some("lean-4.36.0".to_owned())]);
        assert!(!root.join(".lake/build").exists());
        assert!(!root.join(".lake/lean-rs-build-cache").exists());
        assert_eq!(builder.probe_ready().expect("selected ready probe"), Some(built));
        assert!(
            CargoLeanCapability::new(root, "MyCapability")
                .package("my_pkg")
                .probe_ready()
                .expect("default probe")
                .is_none()
        );
    }

    #[test]
    fn generated_output_key_rejects_arbitrary_paths_before_fake_lake() {
        for key in [
            "",
            ".",
            "..",
            ".hidden",
            "trailing.",
            "Uppercase",
            "../escape",
            "/absolute",
            "two/parts",
            "bad key",
        ] {
            let fixture = make_generated_output_project(&format!("invalid-{}", key.len()), "MyCapability");
            let root = &fixture.0;
            let mut runner = FakeLake::new(FakeMode::SuccessModern);
            let error = CargoLeanCapability::new(root, "MyCapability")
                .generated_output_key(key)
                .build_with_runner(&mut runner, CargoMetadata::Suppress)
                .expect_err("invalid generated output key");
            assert!(
                matches!(error, LinkDiagnostics::LakeOutputUnresolved { reason, .. } if reason.contains("generated output key"))
            );
            assert_eq!(runner.calls(), 0);
            assert!(!root.join(".lake/lean-rs-generated").exists());
        }
    }

    #[test]
    fn generated_output_refuses_aliases_and_never_falls_back_to_default_outputs() {
        struct DefaultOnlyLake;
        impl LakeRunner for DefaultOnlyLake {
            fn build_shared(
                &mut self,
                project_root: &Path,
                target_name: &str,
                _options: &LakeBuildOptions,
            ) -> Result<LakeRun, std::io::Error> {
                let dylib = project_root
                    .join(".lake/build/lib")
                    .join(format!("libmy__pkg_{target_name}.{}", dylib_ext()));
                write_file(&dylib, "wrong default output");
                Ok(success_run())
            }
        }

        let fixture = make_generated_output_project("no-default-fallback", "MyCapability");
        let root = &fixture.0;
        CargoLeanCapability::new(root, "MyCapability")
            .package("my_pkg")
            .build_with_runner(&mut FakeLake::new(FakeMode::SuccessModern), CargoMetadata::Suppress)
            .expect("seed default output");
        let selected = CargoLeanCapability::new(root, "MyCapability")
            .package("my_pkg")
            .generated_output_key("lean-4.36.0");
        assert!(selected.probe_ready().expect("keyed probe").is_none());
        let error = selected
            .build_with_runner(&mut DefaultOnlyLake, CargoMetadata::Suppress)
            .expect_err("keyed build cannot accept default output");
        assert!(
            matches!(error, LinkDiagnostics::LakeOutputUnresolved { reason, .. } if reason.contains("expected shared library"))
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let parent = root.join(".lake/lean-rs-generated");
            fs::create_dir_all(&parent).expect("generated parent");
            symlink("../build", parent.join("aliased")).expect("alias default build");
            let error = CargoLeanCapability::new(root, "MyCapability")
                .generated_output_key("aliased")
                .probe_ready()
                .expect_err("aliased generated output rejected");
            assert!(
                matches!(error, LinkDiagnostics::LakeOutputUnresolved { ref reason, .. } if reason.contains("aliased")),
                "unexpected dependency alias error: {error:?}"
            );

            let dependency = root.join("interop");
            fs::create_dir_all(dependency.join(".lake/build")).expect("default dependency output");
            fs::create_dir_all(dependency.join(".lake/lean-rs-generated")).expect("dependency generated parent");
            symlink("../build", dependency.join(".lake/lean-rs-generated/lean-4.36.0"))
                .expect("alias dependency output");
            write_file(
                &root.join("lake-manifest.json"),
                r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[{"type":"path","name":"lean_rs_interop_shims","manifestFile":"lake-manifest.json","inherited":false,"dir":"interop"}],"name":"my_pkg","lakeDir":".lake"}"#,
            );
            write_file(
                &dependency.join("lake-manifest.json"),
                r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[],"name":"lean_rs_interop_shims","lakeDir":".lake"}"#,
            );
            let error = CargoLeanCapability::new(root, "MyCapability")
                .generated_output_key("lean-4.36.0")
                .probe_ready()
                .expect_err("aliased dependency output rejected");
            assert!(
                matches!(error, LinkDiagnostics::LakeOutputUnresolved { ref reason, .. } if reason.contains("aliased")),
                "unexpected dependency alias error: {error:?}"
            );
        }
    }

    #[test]
    fn ready_probe_requires_recorded_dependency_dylibs_and_matching_sysroot() {
        let root = make_project("ready-probe-dependency", "MyCapability");
        let dependency_path = root.join("libdependency.dylib");
        write_file(&dependency_path, "dependency");
        let sysroot = make_fake_sysroot("ready-probe");
        let builder = CargoLeanCapability::new(&root, "MyCapability")
            .lean_sysroot(&sysroot)
            .dependency(LeanLibraryDependency::path(&dependency_path));
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let built = builder
            .clone()
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("fixture dependency build");
        assert_eq!(
            builder.probe_ready().expect("matching explicit sysroot"),
            Some(built.clone())
        );
        assert!(
            CargoLeanCapability::new(&root, "MyCapability")
                .probe_ready()
                .expect("different build options")
                .is_none()
        );
        fs::write(&dependency_path, "different dependency output").expect("replace dependency bytes");
        assert!(
            builder
                .probe_ready()
                .expect("dependency output identity changed")
                .is_none()
        );
        fs::remove_file(&dependency_path).expect("remove dependency dylib");
        assert!(builder.probe_ready().expect("missing dependency").is_none());
        assert!(!dependency_path.exists());
        assert_eq!(runner.calls(), 1);
        fs::remove_file(built.manifest_path()).expect("simulate partial publication");
        // A previous build can have a valid cache entry for the primary target alone.
        for _ in 0..2 {
            let error = builder
                .clone()
                .build_with_runner(&mut runner, CargoMetadata::Suppress)
                .expect_err("unresolved dependency prevents capability publication");
            assert!(matches!(error, LinkDiagnostics::LakeOutputUnresolved { reason, .. }
                if reason.contains(dependency_path.to_str().expect("UTF-8 fixture path"))));
            assert!(!built.manifest_path().exists());
        }
        assert_eq!(runner.calls(), 2, "the second rejection used the primary target cache");
        write_file(&dependency_path, "restored dependency");
        let refreshed = builder
            .clone()
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("publish the restored capability");
        assert_eq!(builder.probe_ready().expect("refreshed dependency"), Some(refreshed));
        assert_eq!(runner.calls(), 3);
    }

    #[test]
    fn cargo_capability_build_quiet_returns_metadata() {
        let root = make_project("cargo-capability", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let dylib = build_lake_target_with_runner(&root, "MyCapability", &mut runner, CargoMetadata::Suppress)
            .expect("build target");
        let built = CargoLeanCapability::new(&root, "MyCapability")
            .package("my_pkg")
            .module("MyCapability")
            .env_var("MY_CAPABILITY_DYLIB")
            .manifest_env_var("MY_CAPABILITY_MANIFEST")
            .export_signature(LeanExportSignature::function(
                "my_capability_u8_identity",
                vec![LeanExportArgAbi::new(LeanExportAbiRepr::U8, LeanExportOwnership::None)],
                LeanExportReturnAbi::new(
                    LeanExportAbiRepr::U8,
                    LeanExportOwnership::None,
                    LeanExportResultConvention::Pure,
                ),
            ))
            .build_with_runner(&mut FakeLake::new(FakeMode::SuccessModern), CargoMetadata::Suppress)
            .expect("cargo helper build");

        assert_eq!(built.dylib_path(), dylib.as_path());
        assert_eq!(built.env_var(), "MY_CAPABILITY_DYLIB");
        assert_eq!(built.manifest_env_var(), "MY_CAPABILITY_MANIFEST");
        assert!(built.manifest_path().is_file());
        assert_eq!(built.package(), "my_pkg");
        assert_eq!(built.module(), "MyCapability");
        assert_eq!(built.target_name(), "MyCapability");
        assert!(built.project_root().is_absolute());

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(built.manifest_path()).expect("read manifest"))
                .expect("manifest is valid JSON");
        assert_eq!(
            manifest.get("schema_version").and_then(serde_json::Value::as_u64),
            Some(u64::from(CAPABILITY_MANIFEST_SCHEMA_VERSION)),
        );
        assert_eq!(
            manifest.get("package").and_then(serde_json::Value::as_str),
            Some("my_pkg")
        );
        assert_eq!(
            manifest.get("module").and_then(serde_json::Value::as_str),
            Some("MyCapability")
        );
        assert_eq!(
            manifest
                .get("primary_dylib")
                .and_then(serde_json::Value::as_str)
                .map(Path::new),
            Some(dylib.as_path()),
        );
        assert!(manifest.get("toolchain_fingerprint").is_some());
        assert_eq!(
            manifest
                .get("exports")
                .and_then(serde_json::Value::as_array)
                .and_then(|exports| exports.first())
                .and_then(|export| export.get("symbol"))
                .and_then(serde_json::Value::as_str),
            Some("my_capability_u8_identity"),
        );
    }

    #[test]
    fn cargo_capability_manifest_records_explicit_dependencies() {
        let root = make_project("cargo-capability-explicit-dependency", "MyCapability");
        let dependency = root.join(".lake").join("build").join("lib").join("libdependency.dylib");
        write_file(&dependency, "dependency dylib");

        let built = CargoLeanCapability::new(&root, "MyCapability")
            .package("my_pkg")
            .module("MyCapability")
            .dependency(
                LeanLibraryDependency::path(&dependency)
                    .export_symbols_for_dependents()
                    .initializer("dependency_pkg", "Dependency"),
            )
            .build_with_runner(&mut FakeLake::new(FakeMode::SuccessModern), CargoMetadata::Suppress)
            .expect("cargo helper build");

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(built.manifest_path()).expect("read manifest"))
                .expect("manifest is valid JSON");
        let dependencies = manifest
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .expect("manifest dependencies array");
        assert_eq!(dependencies.len(), 1);
        let dependency_json = dependencies.first().expect("one dependency");
        assert_eq!(
            dependency_json.get("name").and_then(serde_json::Value::as_str),
            Some("dependency_pkg")
        );
        assert_eq!(
            dependency_json
                .get("dylib_path")
                .and_then(serde_json::Value::as_str)
                .map(Path::new),
            Some(dependency.as_path())
        );
        assert_eq!(
            dependency_json
                .get("export_symbols_for_dependents")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            dependency_json
                .get("initializer")
                .and_then(|initializer| initializer.get("package"))
                .and_then(serde_json::Value::as_str),
            Some("dependency_pkg")
        );
        assert_eq!(
            dependency_json
                .get("initializer")
                .and_then(|initializer| initializer.get("module"))
                .and_then(serde_json::Value::as_str),
            Some("Dependency")
        );
    }

    #[test]
    fn cargo_capability_dependency_without_initializer_uses_stable_file_name() {
        let root = make_project("cargo-capability-explicit-dependency-no-init", "MyCapability");
        let dependency = root.join(".lake").join("build").join("lib").join("libsupport.dylib");
        write_file(&dependency, "dependency dylib");

        let built = CargoLeanCapability::new(&root, "MyCapability")
            .package("my_pkg")
            .module("MyCapability")
            .dependency(LeanLibraryDependency::path(&dependency))
            .build_with_runner(&mut FakeLake::new(FakeMode::SuccessModern), CargoMetadata::Suppress)
            .expect("cargo helper build");

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(built.manifest_path()).expect("read manifest"))
                .expect("manifest is valid JSON");
        let dependency_json = manifest
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .and_then(|dependencies| dependencies.first())
            .expect("one dependency");
        assert_eq!(
            dependency_json.get("name").and_then(serde_json::Value::as_str),
            Some("libsupport")
        );
        assert!(
            dependency_json
                .get("initializer")
                .is_some_and(serde_json::Value::is_null),
            "dependencies without initializers record null initializer"
        );
    }

    #[test]
    fn cargo_capability_build_quiet_passes_explicit_sysroot_to_lake() {
        let root = make_project("cargo-capability-explicit-sysroot", "MyCapability");
        let sysroot = make_fake_sysroot("cargo-capability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);

        let built = CargoLeanCapability::new(&root, "MyCapability")
            .package("my_pkg")
            .module("MyCapability")
            .lean_sysroot(&sysroot)
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("cargo helper build");

        assert_eq!(runner.seen_sysroots(), vec![Some(sysroot.clone())]);
        assert_eq!(runner.seen_threads(), vec![None], "no thread cap set by default");

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(built.manifest_path()).expect("read manifest"))
                .expect("manifest is valid JSON");
        let build_toolchain = manifest
            .get("build_toolchain")
            .expect("manifest records selected build toolchain");
        assert_eq!(
            build_toolchain.get("source").and_then(serde_json::Value::as_str),
            Some("ExplicitSysroot")
        );
        assert_eq!(
            build_toolchain.get("sysroot").and_then(serde_json::Value::as_str),
            Some(sysroot.to_str().expect("test sysroot is UTF-8"))
        );
    }

    #[test]
    fn cargo_capability_build_passes_lean_num_threads_to_lake() {
        let root = make_project("cargo-capability-threads", "MyCapability");
        let sysroot = make_fake_sysroot("cargo-capability-threads");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);

        CargoLeanCapability::new(&root, "MyCapability")
            .package("my_pkg")
            .module("MyCapability")
            .lean_sysroot(&sysroot)
            .lean_num_threads(1)
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("cargo helper build with capped threads");

        assert_eq!(runner.seen_threads(), vec![Some(1)]);
    }

    #[test]
    fn cargo_capability_manifest_uses_selected_sysroot_fingerprint() {
        let Some(alias) = header_identical_alias() else {
            return;
        };
        let root = make_project("cargo-capability-explicit-sysroot-fingerprint", "MyCapability");
        let sysroot = make_fake_sysroot_with_version("cargo-capability-fingerprint", alias);
        let mut runner = FakeLake::new(FakeMode::SuccessModern);

        let built = CargoLeanCapability::new(&root, "MyCapability")
            .package("my_pkg")
            .module("MyCapability")
            .lean_sysroot(&sysroot)
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect("cargo helper build");

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(built.manifest_path()).expect("read manifest"))
                .expect("manifest is valid JSON");
        let fingerprint = manifest
            .get("toolchain_fingerprint")
            .and_then(serde_json::Value::as_object)
            .expect("manifest records toolchain fingerprint");
        assert_eq!(
            fingerprint.get("lean_version").and_then(serde_json::Value::as_str),
            Some(alias)
        );
        assert_eq!(
            fingerprint.get("resolved_version").and_then(serde_json::Value::as_str),
            Some(alias)
        );
        assert_eq!(
            fingerprint.get("header_sha256").and_then(serde_json::Value::as_str),
            Some(crate::LEAN_HEADER_DIGEST)
        );
    }

    #[test]
    fn cargo_capability_explicit_sysroot_does_not_fall_back_to_ambient_discovery() {
        let root = make_project("cargo-capability-invalid-explicit-sysroot", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);

        let err = CargoLeanCapability::new(&root, "MyCapability")
            .package("my_pkg")
            .module("MyCapability")
            .lean_sysroot("/definitely/not/a/lean/sysroot")
            .build_with_runner(&mut runner, CargoMetadata::Suppress)
            .expect_err("invalid explicit sysroot must not fall back to ambient probes");

        match err {
            LinkDiagnostics::MissingLean { tried } => {
                assert!(
                    tried.iter().any(|line| line.contains("explicit_sysroot=")),
                    "diagnostic should name the explicit sysroot probe: {tried:?}",
                );
                assert!(
                    tried.iter().any(|line| line == "PATH lookup disabled"),
                    "ambient PATH probe should be disabled: {tried:?}",
                );
            }
            other => panic!("expected MissingLean, got {other:?}"),
        }
        assert_eq!(runner.calls(), 0, "Lake must not run after invalid explicit sysroot");
    }
}
