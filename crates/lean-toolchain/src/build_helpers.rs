//! Reusable build-script helpers for downstream embedders.
//!
//! Inside this workspace `lean-rs-sys`'s `build.rs` is the single source of
//! `cargo:rustc-link-*` directives — `lean-toolchain` does not call into the
//! helper from its own build script. The helper exists for **downstream
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
//! `link-lib` directives) and load-time (the rpath into the Lean toolchain's
//! `lib/lean` directory) so a consumer binary runs without
//! `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH` set.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::diagnostics::LinkDiagnostics;
use crate::discover::{DiscoverOptions, ToolchainInfo, discover_toolchain};

/// Set once after a successful link-directive emission to make repeat calls
/// (e.g. multiple `build.rs` invocations within one process) cheap and
/// idempotent.
static EMITTED: OnceLock<()> = OnceLock::new();

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
    if EMITTED.get().is_some() {
        return Ok(());
    }

    match discover_toolchain(&DiscoverOptions::default()) {
        Ok(info) => {
            emit_for(&info);
            let _ = EMITTED.set(());
            Ok(())
        }
        Err(diagnostic) => {
            emit_rerun_triggers(None);
            Err(diagnostic)
        }
    }
}

/// Build a Lake `lean_lib` shared-library target and return the produced dylib path.
///
/// `project_root` must be the directory containing `lakefile.lean`. `target_name` is the
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
/// `lean_lib` in `lakefile.lean`, [`LinkDiagnostics::LakeBuildFailed`] if Lake exits
/// unsuccessfully, and [`LinkDiagnostics::LakeOutputUnresolved`] for unreadable manifests,
/// source-set traversal failures, cache write failures, or missing built dylibs.
pub fn build_lake_target(project_root: &Path, target_name: &str) -> Result<PathBuf, LinkDiagnostics> {
    let mut runner = RealLakeRunner;
    build_lake_target_with_runner(project_root, target_name, &mut runner, CargoMetadata::Emit)
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
    build_lake_target_with_runner(project_root, target_name, &mut runner, CargoMetadata::Suppress)
}

/// Build-script helper for shipping a Rust crate with bundled Lean code.
///
/// This is the canonical downstream `build.rs` entry point. It composes
/// [`emit_lean_link_directives_checked`], [`build_lake_target`], and the
/// `cargo:rustc-env=...` directive that carries the built capability path
/// into Rust code at compile time.
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
        }
    }

    /// Set the Lake package name used by the module initializer.
    ///
    /// If omitted, the helper infers the package from `lake-manifest.json` or
    /// `lakefile.lean`, matching [`build_lake_target`].
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

    /// Emit link directives, build the Lake shared library, and emit the
    /// `cargo:rustc-env` directive for the built dylib.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics`] if Lean cannot be discovered, Lake cannot
    /// build the target, or the target output cannot be resolved.
    pub fn build(self) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        emit_lean_link_directives_checked()?;
        let dylib_path = build_lake_target(&self.project_root, &self.target_name)?;
        self.finish(dylib_path, CargoMetadata::Emit)
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
        let dylib_path = build_lake_target_quiet(&self.project_root, &self.target_name)?;
        self.finish(dylib_path, CargoMetadata::Suppress)
    }

    fn finish(
        self,
        dylib_path: PathBuf,
        cargo_metadata: CargoMetadata,
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
        cargo_metadata.println(format_args!("cargo:rustc-env={env_var}={}", dylib_path.display()));
        Ok(BuiltLeanCapability {
            dylib_path,
            env_var,
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
    fn build_shared(&mut self, project_root: &Path, target_name: &str) -> Result<LakeRun, std::io::Error>;
}

struct RealLakeRunner;

impl LakeRunner for RealLakeRunner {
    fn build_shared(&mut self, project_root: &Path, target_name: &str) -> Result<LakeRun, std::io::Error> {
        let output = Command::new("lake")
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

fn build_lake_target_with_runner(
    project_root: &Path,
    target_name: &str,
    runner: &mut impl LakeRunner,
    cargo_metadata: CargoMetadata,
) -> Result<PathBuf, LinkDiagnostics> {
    let project_root = fs::canonicalize(project_root).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not canonicalize project root {} ({err})", project_root.display()),
    })?;
    let lakefile = project_root.join("lakefile.lean");
    cargo_metadata.println(format_args!("cargo:rerun-if-changed={}", lakefile.display()));
    let lakefile_toml = project_root.join("lakefile.toml");
    if lakefile_toml.is_file() {
        cargo_metadata.println(format_args!("cargo:rerun-if-changed={}", lakefile_toml.display()));
    }
    let toolchain_file = project_root.join("lean-toolchain");
    if toolchain_file.is_file() {
        cargo_metadata.println(format_args!("cargo:rerun-if-changed={}", toolchain_file.display()));
    }

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
    let initial_cache_key = cache_key(target_name, &package_name, &manifest_digest, &source_set);
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
        .build_shared(&project_root, target_name)
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
    let final_cache_key = cache_key(target_name, &final_package_name, &final_manifest_digest, &source_set);

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
    let quoted = format!("lean_lib «{target_name}»");
    let bare = format!("lean_lib {target_name}");
    let string = format!("lean_lib \"{target_name}\"");
    Ok(contents.contains(&quoted) || contents.contains(&bare) || contents.contains(&string))
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
            package_name_from_lakefile(project_root, target_name, &project_root.join("lakefile.lean"))
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

fn cache_key(target_name: &str, package_name: &str, manifest_digest: &str, source_set: &SourceSet) -> String {
    format!(
        "target={target_name}\npackage={package_name}\nmanifest={manifest_digest}\nsource_count={}\nsource_max_mtime_ns={}\n",
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
        CargoLeanCapability, CargoMetadata, LakeRun, LakeRunner, build_lake_target_with_runner, capability_env_var,
        command_detail,
    };
    use crate::LinkDiagnostics;
    use std::cell::Cell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    #[derive(Clone)]
    struct FakeLake {
        calls: Rc<Cell<usize>>,
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
                mode,
            }
        }

        fn calls(&self) -> usize {
            self.calls.get()
        }
    }

    impl LakeRunner for FakeLake {
        fn build_shared(&mut self, project_root: &Path, target_name: &str) -> Result<LakeRun, std::io::Error> {
            self.calls.set(self.calls.get().saturating_add(1));
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

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
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
    fn cargo_capability_build_quiet_returns_metadata() {
        let root = make_project("cargo-capability", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let dylib = build_lake_target_with_runner(&root, "MyCapability", &mut runner, CargoMetadata::Suppress)
            .expect("build target");
        let built = CargoLeanCapability::new(&root, "MyCapability")
            .package("my_pkg")
            .module("MyCapability")
            .env_var("MY_CAPABILITY_DYLIB")
            .build_quiet()
            .expect("cargo helper build");

        assert_eq!(built.dylib_path(), dylib.as_path());
        assert_eq!(built.env_var(), "MY_CAPABILITY_DYLIB");
        assert_eq!(built.package(), "my_pkg");
        assert_eq!(built.module(), "MyCapability");
        assert_eq!(built.target_name(), "MyCapability");
        assert!(built.project_root().is_absolute());
    }
}
