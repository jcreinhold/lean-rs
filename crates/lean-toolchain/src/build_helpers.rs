//! Build-script helpers for crates that ship Lean code.
//!
//! [`CargoLeanCapability`] asks Lake to build one `lean_lib`'s shared library,
//! writes the capability manifest that `lean-rs` reads at run time and, from
//! a build script, prints the Cargo directives that link the Lean runtime and
//! hand both paths to the crate:
//!
//! ```ignore
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
//!         .package("my_app")
//!         .module("MyCapability")
//!         .build()?;
//!     Ok(())
//! }
//! ```
//!
//! That one call covers link time (`cargo:rustc-link-search` and
//! `cargo:rustc-link-lib`), load time (an rpath to the toolchain's `lib/lean`
//! directory, so the binary needs no `DYLD_FALLBACK_LIBRARY_PATH` or
//! `LD_LIBRARY_PATH`) and the manifest. Lake names and places every library;
//! this module asks it through [`LakeWorkspace`] and builds no Lake path.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::diagnostics::LinkDiagnostics;
use crate::discover::{DiscoverOptions, ToolchainInfo, discover_toolchain};
use crate::fingerprint::ToolchainFingerprint;
use crate::lake::{LakeRunner, LakeWorkspace, RealLakeRunner};
use crate::loader::{LeanExportSignature, LeanLibraryDependency};

/// The `lean_lib` of the `lean-rs-interop-shims` package, which capabilities may import.
const INTEROP_LIBRARY: &str = "LeanRsInterop";

/// The Lake package of `lean-rs-interop-shims`, which its module initializer names.
const INTEROP_PACKAGE: &str = "lean_rs_interop_shims";

/// The lakefile option the lean-rs shim packages read to choose a versioned build directory.
const GENERATED_SHIMS_OPTION: &str = "leanRsGeneratedShims";

/// The versioned output directory the lean-rs shim packages use with one exact Lean sysroot.
///
/// Under Lean 4.36.0 the shim packages build into a directory named
/// `lean-4.36.0`, the key [`CargoLeanCapability::generated_output_key`]
/// takes. Under earlier releases they use Lake's default, and this returns
/// `None`. It checks the exact version and the installed header's digest,
/// and runs neither Lean nor Lake.
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
    let Some(entry) = lean_rs_abi::supported_by_digest(&hex(&Sha256::digest(bytes))) else {
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

/// Build a Lake `lean_lib`'s shared library from a build script and return its path.
///
/// This runs `lake query <target_name>:shared` in `project_root`, the
/// directory holding `lakefile.lean` or `lakefile.toml`. Lake builds the
/// library only when it is out of date, then prints its path. Afterwards this
/// prints `cargo:rerun-if-changed` for each regular file at the top of
/// `project_root` and each `.lean` file below it, skipping directories whose
/// names start with `.`. Lake's own output goes to captured pipes, so stdout
/// carries only Cargo directives. Code outside a build script calls
/// [`LakeWorkspace::shared_library`] instead.
///
/// # Errors
///
/// Returns the errors of [`LakeWorkspace::shared_library`], and
/// [`LinkDiagnostics::LakeOutputUnresolved`] when the source files cannot be
/// listed.
pub fn build_lake_target(project_root: &Path, target_name: &str) -> Result<PathBuf, LinkDiagnostics> {
    let library = LakeWorkspace::new(project_root).shared_library(target_name)?;
    emit_source_triggers(project_root, target_name)?;
    Ok(library)
}

/// Build-script helper for shipping a Rust crate with bundled Lean code.
///
/// This is the downstream `build.rs` entry point. It composes
/// [`emit_lean_link_directives_checked`], a Lake build of one library, the
/// capability manifest, and `cargo:rustc-env` directives that carry the
/// manifest and library paths into Rust code at compile time.
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
    export_signatures: Vec<LeanExportSignature>,
    dependencies: Vec<LeanLibraryDependency>,
}

impl CargoLeanCapability {
    /// Create a build helper for a Lake workspace root and one of its `lean_lib` targets.
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
            export_signatures: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    /// Set the Lake package name, which the module initializer uses.
    ///
    /// Required. Lake prints no package name lean-toolchain can use, so
    /// [`Self::build`], [`Self::build_quiet`] and [`Self::probe_ready`] fail
    /// without running Lake when it is unset.
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
    /// `sysroot` is the Lean prefix containing `include/lean/lean.h` and
    /// `bin/lake`. [`Self::build`] uses it for link-directive discovery, and
    /// every Lake command runs `<sysroot>/bin/lake` with `LEAN_SYSROOT` set
    /// to it. The parent process environment does not change.
    #[must_use]
    pub fn lean_sysroot(mut self, sysroot: impl Into<PathBuf>) -> Self {
        self.lean_sysroot = Some(sysroot.into());
        self
    }

    /// Cap the parallelism of each Lake command by setting `LEAN_NUM_THREADS`.
    ///
    /// See [`LakeWorkspace::lean_num_threads`]. The parent process
    /// environment does not change.
    #[must_use]
    pub fn lean_num_threads(mut self, threads: u32) -> Self {
        self.lean_num_threads = Some(threads);
        self
    }

    /// Mark the library as lean-rs shim output for one versioned directory, such as `lean-4.36.0`.
    ///
    /// The key is one lowercase ASCII path component; see
    /// [`generated_output_key_for_sysroot`]. With a key, each Lake command
    /// gets `-KleanRsGeneratedShims=true`, which the lean-rs shim lakefiles
    /// read to choose their build directory, and every library Lake reports
    /// must lie below a directory named by the key. Lake reuses a cached
    /// configuration without checking the options that made it, so when a
    /// library lies elsewhere a build has Lake read its lakefiles again once
    /// (`-R`), and a probe reports the library as not ready. The libraries
    /// and the manifest must be regular files with one hard link and no
    /// symlink in their path, and the manifest sits beside the library even
    /// when `OUT_DIR` is set.
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
    /// callers do not need to edit manifest JSON after the build. The manifest
    /// lists these after the workspace's `LeanRsInterop` library, which it
    /// records itself whenever Lake knows one.
    #[must_use]
    pub fn dependency(mut self, dependency: LeanLibraryDependency) -> Self {
        self.dependencies.push(dependency);
        self
    }

    /// Emit link directives, have Lake build the library, write the manifest,
    /// and emit `cargo:rustc-env` directives for the manifest and library.
    ///
    /// Lake runs `query <target>:shared`, then `query LeanRsInterop:shared`
    /// unless the target is that library; a workspace without it records no
    /// interop dependency. Afterwards this prints `cargo:rerun-if-changed`
    /// for each regular file at the top of the project root and each `.lean`
    /// file below it, skipping directories whose names start with `.`.
    ///
    /// # Errors
    ///
    /// Returns [`LinkDiagnostics`] if Lean cannot be discovered, the package
    /// name is unset, Lake fails or has no such library, or the manifest
    /// cannot be written.
    pub fn build(self) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        self.build_with_runner(&mut RealLakeRunner, CargoMetadata::Emit)
    }

    /// Same as [`Self::build`] without printing Cargo directives.
    ///
    /// For library and test code. Build scripts should use [`Self::build`]
    /// so Cargo sees the link and rerun directives.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::build`]; Lean discovery runs only for an
    /// explicit sysroot.
    pub fn build_quiet(self) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        self.build_with_runner(&mut RealLakeRunner, CargoMetadata::Suppress)
    }

    /// Return the capability if Lake reports its libraries up to date and the manifest matches.
    ///
    /// This runs the queries of [`Self::build`] with `--no-build`. A library
    /// Lake would have to build gives `Ok(None)`, as do a missing or
    /// different manifest and a missing dependency file. The probe builds no
    /// library and writes no manifest, but Lake loads the workspace for each
    /// query and may write its own files, such as its configuration cache and
    /// `lake-manifest.json`, or fetch missing dependencies. Nothing may build
    /// the project between this probe and loading its result.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`LakeWorkspace::built_shared_library`],
    /// discovery errors for an explicit sysroot, and
    /// [`LinkDiagnostics::LakeOutputUnresolved`] when the package name is
    /// unset, a keyed file is aliased, or the manifest cannot be read.
    pub fn probe_ready(&self) -> Result<Option<BuiltLeanCapability>, LinkDiagnostics> {
        self.probe_ready_with(&mut RealLakeRunner)
    }

    fn build_with_runner(
        self,
        runner: &mut impl LakeRunner,
        cargo_metadata: CargoMetadata,
    ) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        let package = self.checked_package()?.to_owned();
        let discover_options = self.discover_options();
        let selected_toolchain = match cargo_metadata {
            CargoMetadata::Emit => Some(emit_lean_link_directives_checked_with_options(&discover_options)?),
            CargoMetadata::Suppress if self.lean_sysroot.is_some() => Some(discover_toolchain(&discover_options)?),
            CargoMetadata::Suppress => None,
        };
        let mut answers = self.build_answers(runner, &self.workspace(false))?;
        if self.outside_slot(&answers).is_some() {
            answers = self.build_answers(runner, &self.workspace(true))?;
            if let Some(library) = self.outside_slot(&answers) {
                return Err(self.unresolved(format!(
                    "Lake built {} outside a directory named by the generated output key",
                    library.display()
                )));
            }
        }
        self.check_slot_files(answers.libraries())?;
        self.finish(answers, package, cargo_metadata, selected_toolchain.as_ref())
    }

    fn probe_ready_with(&self, runner: &mut impl LakeRunner) -> Result<Option<BuiltLeanCapability>, LinkDiagnostics> {
        let package = self.checked_package()?;
        let Some(answers) = self.ready_answers(runner, &self.workspace(false))? else {
            return Ok(None);
        };
        if self.outside_slot(&answers).is_some() {
            return Ok(None);
        }
        self.check_slot_files(answers.libraries())?;
        let manifest_path = self.manifest_path(&answers.library);
        let bytes = match fs::read(&manifest_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(self.unresolved(format!("could not read {} ({error})", manifest_path.display())));
            }
        };
        self.check_slot_files([manifest_path.as_path()])?;
        let Ok(actual) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Ok(None);
        };
        // Explicit discovery consults only the given sysroot, never PATH,
        // elan or Lake. Without one, build_quiet records this crate's own
        // toolchain fingerprint.
        let selected = if self.lean_sysroot.is_some() {
            Some(discover_toolchain(&self.discover_options())?)
        } else {
            None
        };
        let project_root = self.canonical_root()?;
        let names = self.names();
        let expected = capability_manifest_value(
            &project_root,
            &self.target_name,
            package,
            &names,
            &answers,
            &self.export_signatures,
            &self.dependencies,
            selected.as_ref(),
        )?;
        if actual != expected {
            return Ok(None);
        }
        match check_manifest_dependency_files(&expected) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.unresolved(error.to_string())),
        }
        Ok(Some(BuiltLeanCapability {
            dylib_path: answers.library,
            env_var: names.env_var,
            manifest_path,
            manifest_env_var: names.manifest_env_var,
            package: package.to_owned(),
            module: names.module,
            target_name: self.target_name.clone(),
            project_root,
        }))
    }

    /// Check the key's form and return the package name, both before any Lake command.
    fn checked_package(&self) -> Result<&str, LinkDiagnostics> {
        if let Some(key) = self.generated_output_key.as_deref()
            && !valid_output_key(key)
        {
            return Err(self.unresolved(
                "the generated output key must be one lowercase ASCII component of at most 64 bytes without edge dots"
                    .to_owned(),
            ));
        }
        self.package.as_deref().ok_or_else(|| {
            self.unresolved("no Lake package name; set one with `CargoLeanCapability::package`".to_owned())
        })
    }

    /// The workspace and settings each Lake command for this capability uses.
    fn workspace(&self, reconfigure: bool) -> LakeWorkspace {
        let mut workspace = LakeWorkspace::new(&self.project_root);
        if let Some(sysroot) = &self.lean_sysroot {
            workspace = workspace.lean_sysroot(sysroot);
        }
        if let Some(threads) = self.lean_num_threads {
            workspace = workspace.lean_num_threads(threads);
        }
        if self.generated_output_key.is_some() {
            workspace = workspace.config_option(GENERATED_SHIMS_OPTION, "true");
        }
        if reconfigure {
            workspace = workspace.reconfigure();
        }
        workspace
    }

    /// Lake's answers when it may build what is out of date.
    fn build_answers(
        &self,
        runner: &mut impl LakeRunner,
        workspace: &LakeWorkspace,
    ) -> Result<LakeAnswers, LinkDiagnostics> {
        let library = workspace.shared_library_with(runner, &self.target_name)?;
        let interop = if self.target_name == INTEROP_LIBRARY {
            None
        } else {
            optional_library(workspace.shared_library_with(runner, INTEROP_LIBRARY))?
        };
        Ok(LakeAnswers { library, interop })
    }

    /// Lake's answers when it may build nothing; `Ok(None)` when something is out of date.
    fn ready_answers(
        &self,
        runner: &mut impl LakeRunner,
        workspace: &LakeWorkspace,
    ) -> Result<Option<LakeAnswers>, LinkDiagnostics> {
        let Some(library) = workspace.built_shared_library_with(runner, &self.target_name)? else {
            return Ok(None);
        };
        let interop = if self.target_name == INTEROP_LIBRARY {
            None
        } else {
            match optional_library(workspace.built_shared_library_with(runner, INTEROP_LIBRARY))? {
                Some(Some(interop)) => Some(interop),
                Some(None) => return Ok(None),
                None => None,
            }
        };
        Ok(Some(LakeAnswers { library, interop }))
    }

    /// The first library Lake reported outside a directory named by the key.
    fn outside_slot<'a>(&self, answers: &'a LakeAnswers) -> Option<&'a Path> {
        let key = self.generated_output_key.as_deref()?;
        answers.libraries().find(|library| {
            !library
                .ancestors()
                .skip(1)
                .any(|dir| dir.file_name().is_some_and(|name| name == key))
        })
    }

    /// With a key, refuse a file reached through a symlink or holding another hard link.
    fn check_slot_files<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) -> Result<(), LinkDiagnostics> {
        if self.generated_output_key.is_none() {
            return Ok(());
        }
        for path in paths {
            let unaliased = unaliased_file(path)
                .map_err(|error| self.unresolved(format!("could not inspect {} ({error})", path.display())))?;
            if !unaliased {
                return Err(self.unresolved(format!(
                    "{} is not a regular file with one link and no symlink in its path",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    /// Where the manifest goes: beside the library, or in `OUT_DIR` for an
    /// unkeyed build script, so crates that build one project keep their own.
    fn manifest_path(&self, library: &Path) -> PathBuf {
        let name = capability_manifest_name(&self.target_name, &self.export_signatures);
        if self.generated_output_key.is_none()
            && let Some(out_dir) = env::var_os("OUT_DIR")
        {
            return PathBuf::from(out_dir).join(name);
        }
        library.with_file_name(name)
    }

    fn names(&self) -> CapabilityNames {
        CapabilityNames {
            module: self.module.clone().unwrap_or_else(|| self.target_name.clone()),
            env_var: self
                .env_var
                .clone()
                .unwrap_or_else(|| capability_env_var(&self.target_name)),
            manifest_env_var: self
                .manifest_env_var
                .clone()
                .unwrap_or_else(|| capability_manifest_env_var(&self.target_name)),
        }
    }

    fn canonical_root(&self) -> Result<PathBuf, LinkDiagnostics> {
        fs::canonicalize(&self.project_root)
            .map_err(|error| self.unresolved(format!("could not canonicalize the project root ({error})")))
    }

    fn unresolved(&self, reason: String) -> LinkDiagnostics {
        LinkDiagnostics::LakeOutputUnresolved {
            project_root: self.project_root.clone(),
            target_name: self.target_name.clone(),
            reason,
        }
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
        answers: LakeAnswers,
        package: String,
        cargo_metadata: CargoMetadata,
        selected_toolchain: Option<&ToolchainInfo>,
    ) -> Result<BuiltLeanCapability, LinkDiagnostics> {
        let project_root = self.canonical_root()?;
        let names = self.names();
        let manifest_path = self.manifest_path(&answers.library);
        let manifest = capability_manifest_value(
            &project_root,
            &self.target_name,
            &package,
            &names,
            &answers,
            &self.export_signatures,
            &self.dependencies,
            selected_toolchain,
        )?;
        check_manifest_dependency_files(&manifest).map_err(|error| self.unresolved(error.to_string()))?;
        let bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| self.unresolved(format!("could not encode the capability manifest ({error})")))?;
        write_atomic(&manifest_path, &bytes)
            .map_err(|error| self.unresolved(format!("could not write {} ({error})", manifest_path.display())))?;
        if cargo_metadata == CargoMetadata::Emit {
            emit_source_triggers(&project_root, &self.target_name)?;
            println!("cargo:rustc-env={}={}", names.env_var, answers.library.display());
            println!("cargo:rustc-env={}={}", names.manifest_env_var, manifest_path.display());
        }
        Ok(BuiltLeanCapability {
            dylib_path: answers.library,
            env_var: names.env_var,
            manifest_path,
            manifest_env_var: names.manifest_env_var,
            package,
            module: names.module,
            target_name: self.target_name,
            project_root,
        })
    }
}

/// Lake's answers for one capability: its library, and the `LeanRsInterop`
/// library when the workspace has one.
struct LakeAnswers {
    library: PathBuf,
    interop: Option<PathBuf>,
}

impl LakeAnswers {
    fn libraries(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.library.as_path()).chain(self.interop.as_deref())
    }
}

/// The module and Cargo variable names of one capability, after defaults.
struct CapabilityNames {
    module: String,
    env_var: String,
    manifest_env_var: String,
}

/// A library the workspace may lack: Lake's report that it has none becomes `None`.
fn optional_library<T>(answer: Result<T, LinkDiagnostics>) -> Result<Option<T>, LinkDiagnostics> {
    match answer {
        Ok(library) => Ok(Some(library)),
        Err(LinkDiagnostics::LakeTargetMissing { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// One lowercase ASCII path component of at most 64 bytes, without edge dots.
fn valid_output_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && !key.starts_with('.')
        && !key.ends_with('.')
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Whether `path` names a regular file with one hard link, reached through no symlink.
fn unaliased_file(path: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    Ok(metadata.is_file() && has_one_link(&metadata) && fs::canonicalize(path)? == path)
}

#[cfg(unix)]
fn has_one_link(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    metadata.nlink() == 1
}

#[cfg(not(unix))]
fn has_one_link(_metadata: &fs::Metadata) -> bool {
    true
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

/// Print `cargo:rerun-if-changed` for each file whose edit can change what Lake builds.
///
/// These are each regular file at the top of `root`, such as the lakefile,
/// and each `.lean` file below it, skipping directories whose names start
/// with `.`, such as Lake's own. Cargo reruns the build script when one
/// changes, and Lake then decides what to rebuild.
fn emit_source_triggers(root: &Path, target_name: &str) -> Result<(), LinkDiagnostics> {
    let mut paths = Vec::new();
    collect_sources(root, true, &mut paths).map_err(|error| LinkDiagnostics::LakeOutputUnresolved {
        project_root: root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not list the source files ({error})"),
    })?;
    paths.sort();
    for path in paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    Ok(())
}

fn collect_sources(dir: &Path, top: bool, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().as_encoded_bytes().starts_with(b".") {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_sources(&path, false, paths)?;
        } else if file_type.is_file() && (top || path.extension().is_some_and(|extension| extension == "lean")) {
            paths.push(path);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CargoMetadata {
    Emit,
    Suppress,
}

// Check the same ordered dependency list on the read-only probe and immediately
// before publication. Lake can produce some of these files during the build.
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
    names: &CapabilityNames,
    answers: &LakeAnswers,
    export_signatures: &[LeanExportSignature],
    explicit_dependencies: &[LeanLibraryDependency],
    selected_toolchain: Option<&ToolchainInfo>,
) -> Result<serde_json::Value, LinkDiagnostics> {
    let mut dependencies = answers
        .interop
        .iter()
        .map(|interop| {
            serde_json::json!({
                "name": INTEROP_PACKAGE,
                "dylib_path": interop.display().to_string(),
                "export_symbols_for_dependents": true,
                "initializer": {
                    "package": INTEROP_PACKAGE,
                    "module": INTEROP_LIBRARY,
                }
            })
        })
        .collect::<Vec<_>>();
    dependencies.extend(explicit_dependencies.iter().map(lean_library_dependency_to_json));
    let fingerprint = manifest_toolchain_fingerprint(project_root, target_name, selected_toolchain)?;
    let search_dirs = answers
        .library
        .parent()
        .map(|dir| dir.display().to_string())
        .into_iter()
        .collect::<Vec<_>>();
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
        "module": &names.module,
        "primary_dylib": answers.library.display().to_string(),
        "manifest_env_var": &names.manifest_env_var,
        "lean_version": &fingerprint.lean_version,
        "resolved_lean_version": &fingerprint.resolved_version,
        "lean_header_sha256": &fingerprint.header_sha256,
        "toolchain_fingerprint": {
            "lean_version": &fingerprint.lean_version,
            "resolved_version": &fingerprint.resolved_version,
            "header_sha256": &fingerprint.header_sha256,
            "host_triple": fingerprint.host_triple,
        },
        "build_toolchain": build_toolchain,
        "search_dirs": search_dirs,
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
        host_triple: ToolchainFingerprint::current().host_triple,
    })
}

fn sha256_manifest_input(project_root: &Path, target_name: &str, path: &Path) -> Result<String, LinkDiagnostics> {
    let bytes = fs::read(path).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not read selected Lean header {} ({err})", path.display()),
    })?;
    Ok(hex(&Sha256::digest(bytes)))
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

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Output;

    use super::{
        CAPABILITY_MANIFEST_SCHEMA_VERSION, CargoLeanCapability, CargoMetadata, INTEROP_LIBRARY, capability_env_var,
        capability_manifest_env_var, capability_manifest_name, collect_sources, generated_output_key_for_sysroot,
    };
    use crate::lake::tests::{FakeLake, answer, fake_project, temp_dir};
    use crate::{
        LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
        LeanExportSignature, LeanLibraryDependency, LinkDiagnostics,
    };

    type TestResult = Result<(), String>;

    fn text(error: impl std::fmt::Display) -> String {
        error.to_string()
    }

    /// Write `contents` to `path`, creating its parent directories.
    fn write(path: &Path, contents: &str) -> TestResult {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(text)?;
        }
        fs::write(path, contents).map_err(text)
    }

    fn read_manifest(path: &Path) -> Result<serde_json::Value, String> {
        serde_json::from_slice(&fs::read(path).map_err(text)?).map_err(text)
    }

    /// The JSON value at `pointer`, or `null` when there is none.
    fn field(value: &serde_json::Value, pointer: &str) -> serde_json::Value {
        value.pointer(pointer).cloned().unwrap_or_default()
    }

    /// A fake Lake that knows `libraries` and reports every other name as unknown.
    ///
    /// While `stale` is set, a `--no-build` query for a known library exits 3.
    fn lake_knowing(libraries: Vec<(&'static str, PathBuf)>, stale: bool) -> FakeLake<impl FnMut(&[String]) -> Output> {
        FakeLake::new(move |args: &[String]| {
            let target = args
                .last()
                .and_then(|spec| spec.strip_suffix(":shared"))
                .unwrap_or_default();
            match libraries.iter().find(|(name, _)| *name == target) {
                Some(_) if stale && args.iter().any(|arg| arg == "--no-build") => answer(3, "", ""),
                Some((_, path)) => answer(0, &format!("{}\n", path.display()), ""),
                None => answer(1, "", &format!("error: unknown target `{target}`")),
            }
        })
    }

    /// A Lean prefix holding this build's `lean.h` and the given version string.
    fn fake_sysroot(name: &str, version: &str) -> Result<PathBuf, String> {
        let root = temp_dir(name)?;
        let include = root.join("include/lean");
        fs::create_dir_all(&include).map_err(text)?;
        fs::copy(crate::LEAN_HEADER_PATH, include.join("lean.h")).map_err(text)?;
        write(
            &include.join("version.h"),
            &format!("#define LEAN_VERSION_STRING \"{version}\"\n"),
        )?;
        Ok(root)
    }

    fn u8_identity(symbol: &str) -> LeanExportSignature {
        LeanExportSignature::function(
            symbol,
            vec![LeanExportArgAbi::new(LeanExportAbiRepr::U8, LeanExportOwnership::None)],
            LeanExportReturnAbi::new(
                LeanExportAbiRepr::U8,
                LeanExportOwnership::None,
                LeanExportResultConvention::Pure,
            ),
        )
    }

    fn call_lines<F>(lake: &FakeLake<F>) -> Vec<String> {
        lake.calls.iter().map(|call| call.args.join(" ")).collect()
    }

    #[test]
    fn the_build_selected_sysroot_gives_its_versioned_directory() -> TestResult {
        let sysroot = Path::new(crate::LEAN_HEADER_PATH)
            .ancestors()
            .nth(3)
            .ok_or("the build-selected header has no sysroot")?;
        let expected = (crate::LEAN_VERSION == "4.36.0").then_some("lean-4.36.0");
        assert_eq!(generated_output_key_for_sysroot(sysroot).map_err(text)?, expected);
        Ok(())
    }

    #[test]
    fn a_build_writes_the_manifest_for_the_library_lake_names() -> TestResult {
        let (root, _) = fake_project("build")?;
        let library = root.join("out/lib/libdemo_BuildCapability.dylib");
        write(&library, "")?;
        let mut lake = lake_knowing(vec![("BuildCapability", library.clone())], false);
        let built = CargoLeanCapability::new(&root, "BuildCapability")
            .package("demo")
            .module("Build.Capability")
            .env_var("BUILD_CAPABILITY_DYLIB")
            .manifest_env_var("BUILD_CAPABILITY_MANIFEST")
            .export_signature(u8_identity("build_capability_identity"))
            .build_with_runner(&mut lake, CargoMetadata::Suppress)
            .map_err(text)?;
        assert_eq!(built.dylib_path(), library);
        assert_eq!(built.env_var(), "BUILD_CAPABILITY_DYLIB");
        assert_eq!(built.manifest_env_var(), "BUILD_CAPABILITY_MANIFEST");
        assert_eq!(
            (built.package(), built.module(), built.target_name()),
            ("demo", "Build.Capability", "BuildCapability")
        );
        assert_eq!(built.project_root(), root);
        let manifest = read_manifest(built.manifest_path())?;
        assert_eq!(field(&manifest, "/schema_version"), CAPABILITY_MANIFEST_SCHEMA_VERSION);
        assert_eq!(field(&manifest, "/package"), "demo");
        assert_eq!(field(&manifest, "/module"), "Build.Capability");
        assert_eq!(field(&manifest, "/primary_dylib"), library.display().to_string());
        assert_eq!(
            field(&manifest, "/search_dirs"),
            serde_json::json!([root.join("out/lib").display().to_string()])
        );
        assert_eq!(field(&manifest, "/dependencies"), serde_json::json!([]));
        assert_eq!(field(&manifest, "/exports/0/symbol"), "build_capability_identity");
        assert_eq!(
            call_lines(&lake),
            ["query BuildCapability:shared", "query LeanRsInterop:shared"]
        );
        Ok(())
    }

    #[test]
    fn the_interop_library_comes_first_among_dependencies() -> TestResult {
        let (root, _) = fake_project("interop")?;
        let library = root.join("out/lib/libdemo_InteropUser.dylib");
        let interop = root.join("interop/out/lib/libinterop.dylib");
        let support = root.join("support/libsupport.dylib");
        for path in [&library, &interop, &support] {
            write(path, "")?;
        }
        let mut lake = lake_knowing(
            vec![("InteropUser", library), (INTEROP_LIBRARY, interop.clone())],
            false,
        );
        let built = CargoLeanCapability::new(&root, "InteropUser")
            .package("demo")
            .dependency(
                LeanLibraryDependency::path(&support)
                    .export_symbols_for_dependents()
                    .initializer("support_pkg", "Support"),
            )
            .dependency(LeanLibraryDependency::path(&support))
            .build_with_runner(&mut lake, CargoMetadata::Suppress)
            .map_err(text)?;
        let support = support.display().to_string();
        assert_eq!(
            field(&read_manifest(built.manifest_path())?, "/dependencies"),
            serde_json::json!([
                {
                    "name": "lean_rs_interop_shims",
                    "dylib_path": interop.display().to_string(),
                    "export_symbols_for_dependents": true,
                    "initializer": {"package": "lean_rs_interop_shims", "module": "LeanRsInterop"},
                },
                {
                    "name": "support_pkg",
                    "dylib_path": support,
                    "export_symbols_for_dependents": true,
                    "initializer": {"package": "support_pkg", "module": "Support"},
                },
                {
                    "name": "libsupport",
                    "dylib_path": support,
                    "export_symbols_for_dependents": false,
                    "initializer": null,
                },
            ])
        );
        Ok(())
    }

    #[test]
    fn the_interop_library_records_no_dependency_on_itself() -> TestResult {
        let (root, _) = fake_project("interop-self")?;
        let interop = root.join("out/lib/libinterop.dylib");
        write(&interop, "")?;
        let mut lake = lake_knowing(vec![(INTEROP_LIBRARY, interop)], false);
        let built = CargoLeanCapability::new(&root, INTEROP_LIBRARY)
            .package("lean_rs_interop_shims")
            .build_with_runner(&mut lake, CargoMetadata::Suppress)
            .map_err(text)?;
        assert_eq!(
            field(&read_manifest(built.manifest_path())?, "/dependencies"),
            serde_json::json!([])
        );
        assert_eq!(call_lines(&lake), ["query LeanRsInterop:shared"]);
        Ok(())
    }

    #[test]
    fn an_explicit_sysroot_reaches_lake_and_the_manifest() -> TestResult {
        let (root, _) = fake_project("sysroot")?;
        let sysroot = fake_sysroot("sysroot-prefix", crate::LEAN_VERSION)?;
        let library = root.join("out/lib/libdemo_SysrootCapability.dylib");
        write(&library, "")?;
        let mut lake = lake_knowing(vec![("SysrootCapability", library)], false);
        let built = CargoLeanCapability::new(&root, "SysrootCapability")
            .package("demo")
            .lean_sysroot(&sysroot)
            .lean_num_threads(1)
            .build_with_runner(&mut lake, CargoMetadata::Suppress)
            .map_err(text)?;
        assert_eq!(lake.calls.len(), 2);
        for call in &lake.calls {
            assert_eq!(call.program, sysroot.join("bin/lake"));
            for env in [
                ("LEAN_SYSROOT".to_owned(), Some(sysroot.display().to_string())),
                ("LEAN_NUM_THREADS".to_owned(), Some("1".to_owned())),
            ] {
                assert!(call.envs.contains(&env), "missing {env:?} in {:?}", call.envs);
            }
        }
        let manifest = read_manifest(built.manifest_path())?;
        assert_eq!(field(&manifest, "/build_toolchain/source"), "ExplicitSysroot");
        assert_eq!(
            field(&manifest, "/build_toolchain/sysroot"),
            sysroot.display().to_string()
        );
        Ok(())
    }

    #[test]
    fn the_manifest_uses_the_selected_sysroots_fingerprint() -> TestResult {
        let alias = match crate::LEAN_VERSION {
            "4.31.0-rc1" => "4.31.0-rc2",
            "4.31.0-rc2" => "4.31.0-rc1",
            _ => return Ok(()),
        };
        let (root, _) = fake_project("fingerprint")?;
        let sysroot = fake_sysroot("fingerprint-prefix", alias)?;
        let library = root.join("out/lib/libdemo_FingerprintCapability.dylib");
        write(&library, "")?;
        let built = CargoLeanCapability::new(&root, "FingerprintCapability")
            .package("demo")
            .lean_sysroot(&sysroot)
            .build_with_runner(
                &mut lake_knowing(vec![("FingerprintCapability", library)], false),
                CargoMetadata::Suppress,
            )
            .map_err(text)?;
        let manifest = read_manifest(built.manifest_path())?;
        assert_eq!(field(&manifest, "/toolchain_fingerprint/lean_version"), alias);
        assert_eq!(field(&manifest, "/toolchain_fingerprint/resolved_version"), alias);
        assert_eq!(
            field(&manifest, "/toolchain_fingerprint/header_sha256"),
            crate::LEAN_HEADER_DIGEST
        );
        Ok(())
    }

    #[test]
    fn an_explicit_sysroot_never_falls_back_to_ambient_discovery() -> TestResult {
        let (root, _) = fake_project("bad-sysroot")?;
        let mut lake = lake_knowing(Vec::new(), false);
        let result = CargoLeanCapability::new(&root, "BadSysroot")
            .package("demo")
            .lean_sysroot("/definitely/not/a/lean/sysroot")
            .build_with_runner(&mut lake, CargoMetadata::Suppress);
        let Err(LinkDiagnostics::MissingLean { tried }) = result else {
            return Err(format!("expected MissingLean, got {result:?}"));
        };
        assert!(tried.iter().any(|line| line.contains("explicit_sysroot=")), "{tried:?}");
        assert!(tried.iter().any(|line| line == "PATH lookup disabled"), "{tried:?}");
        assert!(lake.calls.is_empty());
        Ok(())
    }

    #[test]
    fn a_capability_without_a_package_runs_no_lake() -> TestResult {
        let (root, _) = fake_project("no-package")?;
        let mut lake = lake_knowing(Vec::new(), false);
        let capability = CargoLeanCapability::new(&root, "NoPackage");
        let probe = capability.probe_ready_with(&mut lake).map(drop);
        let build = capability
            .build_with_runner(&mut lake, CargoMetadata::Suppress)
            .map(drop);
        for result in [probe, build] {
            assert!(
                matches!(result, Err(LinkDiagnostics::LakeOutputUnresolved { .. })),
                "{result:?}"
            );
        }
        assert!(lake.calls.is_empty());
        Ok(())
    }

    #[test]
    fn lakes_report_of_a_missing_target_passes_through() -> TestResult {
        let (root, _) = fake_project("missing-target")?;
        let result = CargoLeanCapability::new(&root, "Missing")
            .package("demo")
            .build_with_runner(&mut lake_knowing(Vec::new(), false), CargoMetadata::Suppress);
        assert!(
            matches!(&result, Err(LinkDiagnostics::LakeTargetMissing { target_name, .. }) if target_name == "Missing"),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn a_probe_is_ready_only_for_built_libraries_and_the_same_manifest() -> TestResult {
        let (root, _) = fake_project("probe")?;
        let library = root.join("out/lib/libdemo_ProbeCapability.dylib");
        let support = root.join("support/libsupport.dylib");
        for path in [&library, &support] {
            write(path, "")?;
        }
        let capability = CargoLeanCapability::new(&root, "ProbeCapability")
            .package("demo")
            .dependency(LeanLibraryDependency::path(&support));
        let libraries = vec![("ProbeCapability", library)];
        let mut stale = lake_knowing(libraries.clone(), true);
        assert_eq!(capability.probe_ready_with(&mut stale).map_err(text)?, None);
        assert_eq!(call_lines(&stale), ["--no-build query ProbeCapability:shared"]);

        let mut lake = lake_knowing(libraries, false);
        assert_eq!(capability.probe_ready_with(&mut lake).map_err(text)?, None);
        let built = capability
            .clone()
            .build_with_runner(&mut lake, CargoMetadata::Suppress)
            .map_err(text)?;
        assert_eq!(
            capability.probe_ready_with(&mut lake).map_err(text)?,
            Some(built.clone())
        );
        for contents in ["{}", "not JSON"] {
            fs::write(built.manifest_path(), contents).map_err(text)?;
            assert_eq!(capability.probe_ready_with(&mut lake).map_err(text)?, None);
        }
        capability
            .clone()
            .build_with_runner(&mut lake, CargoMetadata::Suppress)
            .map_err(text)?;
        fs::remove_file(&support).map_err(text)?;
        assert_eq!(capability.probe_ready_with(&mut lake).map_err(text)?, None);
        Ok(())
    }

    #[test]
    fn a_keyed_build_has_lake_read_its_lakefiles_again_once() -> TestResult {
        let (root, _) = fake_project("keyed")?;
        let default = root.join("out/lib/libdemo_KeyedCapability.dylib");
        let keyed = root.join("out/lean-4.36.0/lib/libdemo_KeyedCapability.dylib");
        for path in [&default, &keyed] {
            write(path, "")?;
        }
        // Lake keeps the configuration it cached until `-R` makes it read
        // the lakefile again with the key's option.
        let answers = (default, keyed.clone());
        let mut reconfigured = false;
        let mut lake = FakeLake::new(move |args: &[String]| {
            reconfigured |= args.iter().any(|arg| arg == "-R");
            let library = if reconfigured { &answers.1 } else { &answers.0 };
            if args.last().is_some_and(|spec| spec == "KeyedCapability:shared") {
                answer(0, &format!("{}\n", library.display()), "")
            } else {
                answer(1, "", "error: unknown target `LeanRsInterop`")
            }
        });
        let capability = CargoLeanCapability::new(&root, "KeyedCapability")
            .package("demo")
            .generated_output_key("lean-4.36.0");
        assert_eq!(capability.probe_ready_with(&mut lake).map_err(text)?, None);
        let built = capability
            .clone()
            .build_with_runner(&mut lake, CargoMetadata::Suppress)
            .map_err(text)?;
        assert_eq!(built.dylib_path(), keyed);
        assert_eq!(built.manifest_path().parent(), keyed.parent());
        assert_eq!(capability.probe_ready_with(&mut lake).map_err(text)?, Some(built));
        assert_eq!(
            call_lines(&lake),
            [
                "-KleanRsGeneratedShims=true --no-build query KeyedCapability:shared",
                "-KleanRsGeneratedShims=true --no-build query LeanRsInterop:shared",
                "-KleanRsGeneratedShims=true query KeyedCapability:shared",
                "-KleanRsGeneratedShims=true query LeanRsInterop:shared",
                "-R -KleanRsGeneratedShims=true query KeyedCapability:shared",
                "-R -KleanRsGeneratedShims=true query LeanRsInterop:shared",
                "-KleanRsGeneratedShims=true --no-build query KeyedCapability:shared",
                "-KleanRsGeneratedShims=true --no-build query LeanRsInterop:shared",
            ]
        );
        Ok(())
    }

    #[test]
    fn keyed_libraries_must_not_be_aliased() -> TestResult {
        let (root, _) = fake_project("aliased")?;
        let real = root.join("elsewhere/libdemo_AliasedCapability.dylib");
        write(&real, "")?;
        let slot = root.join("out/lean-4.36.0/lib");
        fs::create_dir_all(&slot).map_err(text)?;
        let symlink = slot.join("libsymlink.dylib");
        std::os::unix::fs::symlink(&real, &symlink).map_err(text)?;
        let hard_link = slot.join("libhardlink.dylib");
        fs::hard_link(&real, &hard_link).map_err(text)?;
        for library in [symlink, hard_link] {
            let mut lake = lake_knowing(vec![("AliasedCapability", library.clone())], false);
            let result = CargoLeanCapability::new(&root, "AliasedCapability")
                .package("demo")
                .generated_output_key("lean-4.36.0")
                .build_with_runner(&mut lake, CargoMetadata::Suppress);
            assert!(
                matches!(result, Err(LinkDiagnostics::LakeOutputUnresolved { .. })),
                "{} gave {result:?}",
                library.display()
            );
        }
        Ok(())
    }

    #[test]
    fn a_malformed_key_runs_no_lake() -> TestResult {
        let (root, _) = fake_project("bad-key")?;
        let long = "x".repeat(65);
        for key in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "Upper",
            ".hidden",
            "trailing.",
            long.as_str(),
        ] {
            let mut lake = lake_knowing(Vec::new(), false);
            let result = CargoLeanCapability::new(&root, "BadKey")
                .package("demo")
                .generated_output_key(key)
                .build_with_runner(&mut lake, CargoMetadata::Suppress);
            assert!(
                matches!(result, Err(LinkDiagnostics::LakeOutputUnresolved { .. })),
                "{key:?} gave {result:?}"
            );
            assert!(lake.calls.is_empty());
        }
        Ok(())
    }

    #[test]
    fn source_triggers_cover_top_level_files_and_lean_sources() -> TestResult {
        let root = temp_dir("sources")?;
        for path in [
            "lakefile.lean",
            "lean-toolchain",
            "Demo.lean",
            "Demo/Basic.lean",
            "Demo/notes.txt",
            "Demo/.hidden/Skip.lean",
            ".cache/Cache.lean",
            ".env",
        ] {
            write(&root.join(path), "")?;
        }
        let mut paths = Vec::new();
        collect_sources(&root, true, &mut paths).map_err(text)?;
        paths.sort();
        let expected = ["Demo/Basic.lean", "Demo.lean", "lakefile.lean", "lean-toolchain"].map(|path| root.join(path));
        assert_eq!(paths, expected);
        Ok(())
    }

    #[test]
    fn capability_env_vars_are_deterministic() {
        assert_eq!(
            capability_env_var("MyCapability"),
            "LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB"
        );
        assert_eq!(
            capability_env_var("demo-tool_index"),
            "LEAN_RS_CAPABILITY_DEMO_TOOL_INDEX_DYLIB"
        );
        assert_eq!(
            capability_manifest_env_var("MyCapability"),
            "LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST"
        );
        assert_eq!(
            capability_manifest_env_var("demo-tool_index"),
            "LEAN_RS_CAPABILITY_DEMO_TOOL_INDEX_MANIFEST"
        );
    }

    #[test]
    fn manifest_names_carry_a_digest_of_non_empty_exports() {
        assert_eq!(
            capability_manifest_name("MyCapability", &[]),
            "MyCapability.lean-rs-capability.json"
        );
        let first = capability_manifest_name("MyCapability", &[u8_identity("my_capability_u8_identity")]);
        let second = capability_manifest_name("MyCapability", &[u8_identity("my_capability_other_identity")]);
        assert_ne!(first, second);
        for name in [first, second] {
            assert!(name.starts_with("MyCapability-"), "{name}");
            assert!(name.ends_with(".lean-rs-capability.json"), "{name}");
        }
    }
}
