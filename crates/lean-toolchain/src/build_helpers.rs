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
use std::io::{self, Write as _};
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
/// - the target name and Lake package name.
///
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
    let mut runner = RealLakeRunner;
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
    let mut runner = RealLakeRunner;
    build_lake_target_with_runner_and_options(
        project_root,
        target_name,
        &mut runner,
        CargoMetadata::Suppress,
        &LakeBuildOptions::default(),
    )
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
            export_signatures: Vec::new(),
            dependencies: Vec::new(),
        }
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

    /// Add multiple dependent Lean dylibs that must be loaded before this
    /// capability.
    #[must_use]
    pub fn dependencies(mut self, dependencies: impl IntoIterator<Item = LeanLibraryDependency>) -> Self {
        self.dependencies.extend(dependencies);
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
        self.build_with_runner(&mut RealLakeRunner, CargoMetadata::Emit)
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
        self.build_with_runner(&mut RealLakeRunner, CargoMetadata::Suppress)
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
        };
        let dylib_path = build_lake_target_with_runner_and_options(
            &self.project_root,
            &self.target_name,
            runner,
            cargo_metadata,
            &lake_options,
        )?;
        self.finish(dylib_path, cargo_metadata, selected_toolchain.as_ref())
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
    fn build_shared(
        &mut self,
        project_root: &Path,
        target_name: &str,
        options: &LakeBuildOptions,
    ) -> Result<LakeRun, std::io::Error>;
}

struct RealLakeRunner;

impl LakeRunner for RealLakeRunner {
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
        let output = command
            .arg("build")
            .arg(format!("{target_name}:shared"))
            .current_dir(project_root)
            .output()?;
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

    let dylib = resolve_dylib_path(&project_root, &package_name, target_name);
    let initial_cache_key = cache_key(target_name, &package_name, &manifest_digest, &source_set, options);
    let cache_path = cache_path(&project_root, target_name);
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
        .map_err(|err| LinkDiagnostics::LakeUnavailable {
            project_root: project_root.clone(),
            target_name: target_name.to_owned(),
            detail: err.to_string(),
        })?;
    if !run.success {
        return Err(LinkDiagnostics::LakeBuildFailed {
            project_root,
            target_name: target_name.to_owned(),
            status: run.status,
            detail: command_detail(&run.stdout, &run.stderr),
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
    );

    let dylib = resolve_dylib_path(&project_root, &final_package_name, target_name);
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
    fs::write(&cache_path, final_cache_key).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
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

fn resolve_dylib_path(project_root: &Path, package_name: &str, target_name: &str) -> PathBuf {
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = project_root.join(".lake").join("build").join("lib");
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

fn cache_path(project_root: &Path, target_name: &str) -> PathBuf {
    project_root
        .join(".lake")
        .join("lean-rs-build-cache")
        .join(format!("{}.cache", sanitize_target_name(target_name)))
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
) -> Result<PathBuf, LinkDiagnostics> {
    let manifest_path = capability_manifest_path(project_root, target_name, export_signatures);
    let mut dependencies = capability_dependencies(project_root, target_name)?;
    dependencies.extend(explicit_dependencies.iter().map(lean_library_dependency_to_json));
    let fingerprint = ToolchainFingerprint::current();
    let search_dirs = capability_search_dirs(project_root, dylib_path);
    let build_toolchain = selected_toolchain.map(|info| {
        serde_json::json!({
            "source": format!("{:?}", info.source),
            "sysroot": info.prefix.display().to_string(),
            "version": &info.version,
            "lean_binary": info.lean_binary.as_ref().map(|path| path.display().to_string()),
        })
    });
    let manifest = serde_json::json!({
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
            "fixture_sha256": &fingerprint.fixture_sha256,
            "host_triple": &fingerprint.host_triple,
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
    });
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
) -> PathBuf {
    let manifest_name = capability_manifest_name(target_name, export_signatures);
    if let Some(out_dir) = env::var_os("OUT_DIR") {
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

fn capability_search_dirs(project_root: &Path, dylib_path: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(parent) = dylib_path.parent() {
        dirs.push(parent.to_path_buf());
    }
    dirs.push(project_root.join(".lake").join("build").join("lib"));
    dirs.sort();
    dirs.dedup();
    dirs
}

fn capability_dependencies(project_root: &Path, target_name: &str) -> Result<Vec<serde_json::Value>, LinkDiagnostics> {
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
        let dylib = resolve_dylib_path(&dependency_root, "lean_rs_interop_shims", "LeanRsInterop");
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
    serde_json::json!({
        "name": dependency
            .module_initializer()
            .map_or_else(|| dependency.path_ref().display().to_string(), |initializer| initializer.package_name().to_owned()),
        "dylib_path": dependency.path_ref().display().to_string(),
        "export_symbols_for_dependents": dependency.exports_symbols_for_dependents(),
        "initializer": initializer,
    })
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

fn cache_key(
    target_name: &str,
    package_name: &str,
    manifest_digest: &str,
    source_set: &SourceSet,
    options: &LakeBuildOptions,
) -> String {
    let sysroot = options
        .lean_sysroot
        .as_deref()
        .map_or("ambient", |path| path.to_str().unwrap_or("<non-utf8-sysroot>"));
    format!(
        "target={target_name}\npackage={package_name}\nmanifest={manifest_digest}\nsource_count={}\nsource_max_mtime_ns={}\nlean_sysroot={sysroot}\n",
        source_set.paths.len(),
        source_set.max_mtime_ns
    )
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
        capability_manifest_env_var, capability_manifest_name, command_detail,
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

    #[derive(Clone)]
    struct FakeLake {
        calls: Rc<Cell<usize>>,
        seen_sysroots: Rc<RefCell<Vec<Option<PathBuf>>>>,
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
                mode,
            }
        }

        fn calls(&self) -> usize {
            self.calls.get()
        }

        fn seen_sysroots(&self) -> Vec<Option<PathBuf>> {
            self.seen_sysroots.borrow().clone()
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
            match self.mode {
                FakeMode::SuccessModern => {
                    let dylib = project_root
                        .join(".lake")
                        .join("build")
                        .join("lib")
                        .join(format!("libmy__pkg_{target_name}.{}", dylib_ext()));
                    write_file(&dylib, "dylib");
                    Ok(success_run())
                }
                FakeMode::SuccessLegacy => {
                    let dylib = project_root
                        .join(".lake")
                        .join("build")
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
        fs::create_dir_all(&root).expect("create temp project");
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
        root
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
        let root = std::env::temp_dir().join(format!("lean-toolchain-sysroot-{}-{name}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        write_file(&root.join("include").join("lean").join("lean.h"), "/* fake lean.h */\n");
        root
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
        write_file(
            &root.join("lakefile.lean"),
            "import Lake\nopen Lake DSL\npackage «my_pkg»\nrequire «lean_rs_interop_shims» from \"../../crates/lean-rs/shims/lean-rs-interop-shims\"\n@[default_target]\nlean_lib «InteropConsumer» where\n  defaultFacets := #[LeanLib.sharedFacet]\n",
        );
        write_file(
            &root.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[{"type":"path","scope":"","name":"lean_rs_interop_shims","manifestFile":"lake-manifest.json","inherited":false,"dir":"../../crates/lean-rs/shims/lean-rs-interop-shims","configFile":"lakefile.lean"}],"name":"my_pkg","lakeDir":".lake"}"#,
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
            .build_quiet()
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
            .build_quiet()
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
