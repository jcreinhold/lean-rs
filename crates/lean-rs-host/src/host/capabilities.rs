//! `LeanCapabilities`—loaded manifest-checked host shims and optional user
//! dylibs.
//!
//! [`LeanCapabilities`] owns the [`lean_rs::module::LeanLibrary`] handles and
//! anchors the checked shim capability that [`crate::host::LeanSession`]
//! resolves typed bindings from:
//!
//! - The **user's capability dylib**, when present, is the artefact the
//!   consumer built with `lake build` and named in
//!   [`crate::host::LeanHost::load_capabilities`]. The host stack initializes
//!   it so imported modules can depend on it; arbitrary user export dispatch
//!   stays in the lower-level `lean-rs` crate.
//! - The **shim dylib** is `liblean__rs__host__shims_LeanRsHostShims.dylib`,
//!   built from the `lean-rs-host` crate's bundled shim sources. It contains
//!   the manifest-declared mandatory and optional `lean_rs_host_*` `@[export]` symbols that
//!   every typed `LeanSession` method dispatches through. Lake does *not*
//!   transitively bundle the shim's `@[export]` symbols into the user's dylib
//!   because `LeanLib.sharedFacet` emits a per-package shared library, not a
//!   transitive merge, so the host stack
//!   loads the host shim manifest, its generic interop dependency, and the
//!   user dylib explicitly. All dylibs share one Lean runtime; each per-module
//!   `initialize_<Module>` short-circuits idempotently on its own flag.
//!
//! A missing or mismatched mandatory shim symbol fails session construction
//! through checked binding resolution; a missing optional shim symbol
//! degrades to a synthesised
//! [`crate::host::meta::LeanMetaResponse::Unsupported`] at the
//! [`crate::LeanSession::run_meta`] call site. Bindings are resolved once
//! per session and then cached as typed call handles—no per-query `dlsym`.
//!
//! Construction goes through either [`crate::host::LeanHost::load_capabilities`]
//! or [`crate::host::LeanHost::load_shims_only`]; [`LeanCapabilities::session`]
//! then imports a module list and returns the long-lived
//! [`crate::host::LeanSession`] handle.

use core::fmt;

use std::path::{Path, PathBuf};

use lean_rs::error::LeanResult;
use lean_rs::module::{LeanLibrary, LoadedHostArtifacts, TrustedHostCapability, TrustedHostImports};

use super::project_initializer::initializer_matches;
use crate::host::bracketed::{LeanBracketedImportRequest, LeanBracketedImportResult};
use crate::host::cancellation::LeanCancellationToken;
use crate::host::host::LeanHost;
use crate::host::progress::LeanProgressSink;
use crate::host::session::{LeanImportProfileMode, LeanImportProfilerOptions, LeanSession, LeanSessionImportProfile};
use crate::host::shim_bindings::host_shim_export_signatures;

const HOST_CARGO_TEST_IMPORT_OVERRIDE: &str = "LEAN_RS_ALLOW_CARGO_TEST_HOST_IMPORTS";

/// Loaded generic interop, host shim, and optional user dylibs with a checked
/// host-shim capability ready for session binding resolution.
///
/// Owns the [`LeanLibrary`] handles so callers do not have to track
/// the dylibs' lifetimes separately. Borrows from the parent
/// [`LeanHost`] for the runtime + project context. Neither [`Send`] nor
/// [`Sync`]: inherited from the contained `LeanLibrary` handles.
pub struct LeanCapabilities<'lean, 'h> {
    host: &'h LeanHost<'lean>,
    /// User's capability dylib—the one named in `load_capabilities`, absent
    /// for `load_shims_only`. Kept alive so initialized user modules remain
    /// loaded while sessions import and query their environments.
    user_library: Option<LeanLibrary<'lean>>,
    /// Manifest-backed bundled host shim capability. Session construction
    /// resolves typed bindings from this checked surface.
    shim_capability: LoadedHostArtifacts<'lean>,
    // Prepared before any project read custody. Import and body paths only
    // read these locations; they must never run a nested Lake writer.
    shim_search_paths: Vec<PathBuf>,
    keyed_project_search_paths: Option<Vec<PathBuf>>,
    artifact_roots: Vec<PathBuf>,
}

impl fmt::Debug for LeanCapabilities<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeanCapabilities").finish_non_exhaustive()
    }
}

impl<'lean, 'h> LeanCapabilities<'lean, 'h> {
    /// Combine prepared import metadata with an application-attested, already loaded host capability.
    ///
    /// # Errors
    ///
    /// Returns an error if the project artifact roots cannot be resolved.
    pub fn from_prepared(
        host: &'h LeanHost<'lean>,
        prepared: &LeanPreparedHostArtifacts,
        shim_capability: LoadedHostArtifacts<'lean>,
    ) -> LeanResult<Self> {
        if !shim_capability.matches_snapshot(
            prepared.manifest_path(),
            prepared.manifest_json().as_bytes(),
            prepared.artifact_roots(),
            prepared.shim_search_paths(),
        ) {
            return Err(lean_rs::__host_internals::host_module_init(
                "prepared host artifacts differ from the snapshot admitted by the native loader",
            ));
        }
        let mut artifact_roots = host.project().artifact_roots()?;
        artifact_roots.extend(prepared.artifact_roots.iter().cloned());
        artifact_roots.sort();
        artifact_roots.dedup();
        let keyed_project_search_paths = prepared
            .project_generated_output_key
            .as_deref()
            .map(|key| host.project().olean_search_paths_for_output(Some(key)));
        Ok(Self {
            host,
            user_library: None,
            shim_capability,
            shim_search_paths: prepared.shim_search_paths.clone(),
            keyed_project_search_paths,
            artifact_roots,
        })
    }

    /// Initialize the requested project module on an application-supplied library.
    ///
    /// The host shim and user library refer to images already admitted by the native loader.
    ///
    /// # Errors
    ///
    /// Returns a project path or module initializer error.
    pub fn from_prepared_capability(
        host: &'h LeanHost<'lean>,
        prepared: &LeanPreparedHostArtifacts,
        shim_capability: LoadedHostArtifacts<'lean>,
        project: (LeanLibrary<'lean>, lean_rs::module::LeanModuleInitializer),
        package: &str,
        lib_name: &str,
    ) -> LeanResult<Self> {
        let mut capabilities = Self::from_prepared(host, prepared, shim_capability)?;
        let (library, approved) = project;
        if !library.opened_for(host.runtime(), &host.project_library_path(package, lib_name))
            || !initializer_matches(&approved, package, lib_name)
        {
            return Err(lean_rs::__host_internals::host_module_init(
                "project loader returned a different image or initializer",
            ));
        }
        library.initialize_module(approved.package_name(), approved.module_name())?;
        capabilities.user_library = Some(library);
        Ok(capabilities)
    }

    /// Import the named modules into a fresh Lean environment and
    /// return a session over the result.
    ///
    /// Imports happen exactly once per `session()` call. The returned
    /// [`LeanSession`] owns the imported environment and reuses this
    /// capability's checked shim bindings for every query.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before import dispatch.
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if the Lean-side
    /// import raises (missing `.olean`, malformed module name, …),
    /// with the bounded message Lean surfaced.
    pub fn session<'c>(
        &'c self,
        imports: &TrustedHostImports<'lean, 'c>,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<LeanSession<'lean, 'c>> {
        reject_same_process_cargo_test_session_import()?;
        LeanSession::import(self, imports, cancellation, progress)
    }

    /// Import using an explicit full-session import profile.
    ///
    /// Use this when a caller intentionally needs a broader import shape than
    /// the default private profile. No profile falls back silently to
    /// another one: import and service failures are reported for the requested
    /// profile.
    ///
    /// # Errors
    ///
    /// Returns a host resource error when same-process cargo-test imports are
    /// blocked, a cancellation error when `cancellation` is already cancelled,
    /// or the Lean/import failure surfaced by the requested profile.
    pub fn session_with_profile<'c>(
        &'c self,
        imports: &TrustedHostImports<'lean, 'c>,
        profile: LeanSessionImportProfile,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<LeanSession<'lean, 'c>> {
        reject_same_process_cargo_test_session_import()?;
        LeanSession::import_with_profile(self, imports, profile, cancellation, progress)
    }

    /// Import using one of the closed diagnostic import modes.
    ///
    /// This exists for profiling import breadth only. Normal host sessions use
    /// [`Self::session`], whose default is [`LeanSessionImportProfile::Private`].
    ///
    /// # Errors
    ///
    /// Returns a host resource error when same-process cargo-test imports are
    /// blocked, or the Lean/import failure surfaced by the requested diagnostic
    /// mode.
    pub fn profiling_session<'c>(
        &'c self,
        imports: &TrustedHostImports<'lean, 'c>,
        mode: LeanImportProfileMode,
        profiler_options: &LeanImportProfilerOptions,
    ) -> LeanResult<LeanSession<'lean, 'c>> {
        reject_same_process_cargo_test_session_import()?;
        LeanSession::import_profiled(self, imports, mode, profiler_options)
    }

    /// Run a one-shot no-extension import query inside Lean's compacted-region
    /// bracket.
    ///
    /// The Lean shim uses `withImportModules`, which imports with
    /// `loadExts := false`, serializes only the requested declaration metadata
    /// plus import stats, frees the imported compacted regions, and then
    /// returns Rust-owned data. It is deliberately not a replacement for
    /// [`Self::session`]: parser, elaboration, proof-state, pretty-printing,
    /// and capability workflows require full sessions with loaded extensions.
    /// Extending this path to return Lean-owned objects would be unsound unless
    /// their lifetime across `Environment.freeRegions` can be proven locally.
    ///
    /// # Errors
    ///
    /// Returns the Lean/import or ABI-conversion failure encountered while
    /// running the closed bracketed query.
    pub fn bracketed_import_query(
        &self,
        imports: &lean_rs::module::TrustedBracketedImports<'lean, '_>,
        request: LeanBracketedImportRequest,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<LeanBracketedImportResult> {
        LeanBracketedImportResult::query(self, imports, request, progress)
    }

    /// The capability's parent host (for runtime + project access by
    /// the session dispatch).
    pub(crate) fn host(&self) -> &'h LeanHost<'lean> {
        self.host
    }

    /// Shared output domains prepared with this loaded capability.
    ///
    /// A worker holds read custody over these roots during imports and body
    /// operations. These paths name storage domains, not loaded ABI evidence.
    #[must_use]
    pub fn artifact_roots(&self) -> &[PathBuf] {
        &self.artifact_roots
    }

    /// Bundled import locations prepared during capability construction.
    pub(crate) fn shim_search_paths(&self) -> &[PathBuf] {
        &self.shim_search_paths
    }

    pub(crate) fn keyed_project_search_paths(&self) -> Option<&[PathBuf]> {
        self.keyed_project_search_paths.as_deref()
    }

    /// Ordered project and shim search paths for an application's import attestation.
    #[must_use]
    pub fn import_search_paths(&self) -> Vec<String> {
        LeanSession::import_search_paths(self)
    }

    /// Manifest-backed bundled host shim capability.
    pub fn shim_capability(&self) -> &TrustedHostCapability<'lean> {
        self.shim_capability.host()
    }
}

/// Runtime-free storage metadata for a prepared bundled host capability.
///
/// A snapshot is not evidence that a runtime loaded these artifacts. Retain
/// cooperating reader custody across validation, dynamic loading, all module
/// initializers and the lifetime of the native worker generation.
#[derive(Clone, Debug)]
pub struct LeanPreparedHostArtifacts {
    manifest_path: PathBuf,
    manifest_json: String,
    artifact_roots: Vec<PathBuf>,
    shim_search_paths: Vec<PathBuf>,
    generated_output_key: Option<String>,
    project_generated_output_key: Option<String>,
}

impl LeanPreparedHostArtifacts {
    /// Bind an exact UTF-8 snapshot and its independently admitted storage domains.
    ///
    /// This performs no build, dynamic loading or module initialization.
    ///
    /// # Errors
    /// Returns malformed snapshot or invalid/unbounded canonical-domain errors.
    pub fn from_snapshot(
        manifest_path: PathBuf,
        manifest_json: String,
        artifact_roots: Vec<PathBuf>,
        shim_search_paths: Vec<PathBuf>,
    ) -> LeanResult<Self> {
        let manifest = lean_toolchain::CapabilityManifest::from_bytes(&manifest_path, manifest_json.as_bytes())
            .map_err(|error| lean_rs::__host_internals::host_module_init(error.message().to_owned()))?;
        if manifest_json.len() > 64 * 1024
            || manifest.package != crate::host::lake::SHIM_PACKAGE_NAME
            || manifest.module != crate::host::lake::SHIM_LIB_NAME
            || artifact_roots.is_empty()
            || artifact_roots.len() > 32
            || shim_search_paths.len() != 2
            || artifact_roots
                .iter()
                .zip(artifact_roots.iter().skip(1))
                .any(|(left, right)| left >= right)
            || artifact_roots
                .iter()
                .any(|root| !root.is_absolute() || root.canonicalize().ok().as_ref() != Some(root))
            || shim_search_paths
                .iter()
                .any(|path| !path.is_absolute() || path.canonicalize().ok().as_ref() != Some(path))
        {
            return Err(lean_rs::__host_internals::host_module_init(
                "invalid prepared host artifact storage descriptor".to_owned(),
            ));
        }
        let generated_output_key = generated_output_key_from_search_paths(&shim_search_paths)?;
        Ok(Self {
            manifest_path,
            manifest_json,
            artifact_roots,
            shim_search_paths,
            generated_output_key,
            project_generated_output_key: None,
        })
    }

    /// Canonical manifest path used as diagnostic context.
    #[must_use]
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }
    /// Exact metadata bytes to validate and consume during loading.
    #[must_use]
    pub fn manifest_json(&self) -> &str {
        &self.manifest_json
    }
    /// Canonical cooperating output domains, sorted and deduplicated.
    #[must_use]
    pub fn artifact_roots(&self) -> &[PathBuf] {
        &self.artifact_roots
    }
    /// Ready import paths for the interop and host shim packages.
    #[must_use]
    pub fn shim_search_paths(&self) -> &[PathBuf] {
        &self.shim_search_paths
    }

    /// Select the validated generated-output layout for this application's imports.
    ///
    /// Normal applications retain ordinary Lake paths. A fixture whose own Lake
    /// config selects the prepared shim slot opts in before dispatch.
    ///
    /// # Errors
    /// Returns an invalid or shim-mismatched key.
    #[doc(hidden)]
    pub fn with_project_generated_output_matching_shims(mut self) -> LeanResult<Self> {
        let key = self.generated_output_key.clone().ok_or_else(|| {
            lean_rs::__host_internals::host_module_init("prepared shims do not select a generated-output key")
        })?;
        self.project_generated_output_key = Some(key);
        Ok(self)
    }

    /// Select the matching generated slot for a verified ordinary capability.
    ///
    /// # Errors
    /// Returns an error for a foreign image or incomplete generated import roots.
    #[doc(hidden)]
    pub fn for_generated_project(
        self,
        host: &LeanHost<'_>,
        capability: &lean_rs::module::TrustedWorkerCapability<'_>,
    ) -> LeanResult<Self> {
        let selected = self.with_project_generated_output_matching_shims()?;
        let key = selected.project_generated_output_key.as_deref().ok_or_else(|| {
            lean_rs::__host_internals::host_module_init("generated project imports have no selected key")
        })?;
        host.project()
            .validate_generated_import_layout(key, capability.library_path())?;
        Ok(selected)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn project_uses_generated_output(&self) -> bool {
        self.project_generated_output_key.is_some()
    }
}

fn generated_output_key_from_search_paths(paths: &[PathBuf]) -> LeanResult<Option<String>> {
    let mut selected: Option<String> = None;
    let mut unkeyed_seen = false;
    for path in paths {
        let output = path
            .file_name()
            .and_then(|part| (part == "lean").then_some(path))
            .and_then(|path| path.parent())
            .filter(|lib| lib.file_name().and_then(|part| part.to_str()) == Some("lib"))
            .and_then(Path::parent);
        let marker = output.and_then(Path::parent).filter(|generated| {
            generated.file_name().and_then(|part| part.to_str()) == Some("lean-rs-generated")
                && generated
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|part| part.to_str())
                    == Some(".lake")
        });
        let key = if marker.is_some() {
            let key = output
                .and_then(Path::file_name)
                .and_then(|component| component.to_str())
                .filter(|key| valid_generated_output_component(key))
                .ok_or_else(|| lean_rs::__host_internals::host_module_init("prepared shim output key is invalid"))?;
            Some(key.to_owned())
        } else {
            None
        };
        match (&selected, key) {
            (None, Some(key)) if !unkeyed_seen => selected = Some(key),
            (Some(current), Some(key)) if current == &key => {}
            (None, None) => unkeyed_seen = true,
            _ => {
                return Err(lean_rs::__host_internals::host_module_init(
                    "prepared shim search paths select mixed output roots",
                ));
            }
        }
    }
    Ok(selected)
}

fn valid_generated_output_component(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key != "."
        && key != ".."
        && !key.starts_with('.')
        && !key.ends_with('.')
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Prepare the bundled host capability without initializing any Lean runtime.
///
/// # Errors
/// Returns the configured writer, build, publication or snapshot validation failure.
pub fn prepare_host_artifacts() -> LeanResult<LeanPreparedHostArtifacts> {
    prepare_host_artifacts_for_output(None)
}

/// Write the bundled shim sources into `destination` and build them there.
///
/// [`prepare_host_artifacts`] builds the shims where Cargo unpacked this crate.
/// An installer calls this instead to give a worker its own copy, which does
/// not change when the Cargo cache does. The sources are the ones this build
/// links against. `destination` must not exist; its parent must. The writer
/// helper named by `LEAN_RS_WRITER_EXECUTABLE` runs Lake.
///
/// # Errors
/// Returns an existing `destination`, a missing writer helper, or a write,
/// build or snapshot failure.
#[cfg(unix)]
pub fn prepare_host_artifacts_in(destination: &std::path::Path) -> LeanResult<LeanPreparedHostArtifacts> {
    let selection = crate::host::lake::ShimOutputSelection::resolve(None)?;
    crate::host::lake::prepare_in(destination, host_shim_export_signatures(), &selection)
}

/// Prepare the bundled host capability in one validated generated-output slot.
///
/// # Errors
/// Returns an invalid output key, writer, build, publication or snapshot validation failure.
#[doc(hidden)]
pub fn prepare_host_artifacts_for_generated_output(
    generated_output_key: &str,
) -> LeanResult<LeanPreparedHostArtifacts> {
    prepare_host_artifacts_for_output(Some(generated_output_key))
}

fn prepare_host_artifacts_for_output(generated_output_key: Option<&str>) -> LeanResult<LeanPreparedHostArtifacts> {
    let selection = crate::host::lake::ShimOutputSelection::resolve(generated_output_key)?;
    let built = crate::host::lake::LakeProject::shim_capability(host_shim_export_signatures(), &selection)?;
    crate::host::lake::prepared_artifacts(&built, selection.key())
}

/// Inspect existing bundled outputs under shared custody without a writer or runtime.
///
/// # Errors
/// Returns reader admission or invalid metadata errors. A cold/stale cache is `None`.
#[cfg(unix)]
pub fn ready_host_artifacts() -> LeanResult<Option<LeanPreparedHostArtifacts>> {
    let selection = crate::host::lake::ShimOutputSelection::resolve(None)?;
    crate::host::lake::ready_artifacts(host_shim_export_signatures(), None, &selection)
}

/// Inspect existing bundled outputs using one explicit private IPC root.
///
/// # Errors
/// Returns reader admission or invalid metadata errors. A cold/stale cache is `None`.
#[cfg(unix)]
#[doc(hidden)]
pub fn ready_host_artifacts_with_ipc_root(ipc_root: &std::path::Path) -> LeanResult<Option<LeanPreparedHostArtifacts>> {
    let selection = crate::host::lake::ShimOutputSelection::resolve(None)?;
    crate::host::lake::ready_artifacts(host_shim_export_signatures(), Some(ipc_root), &selection)
}

/// Inspect one validated generated-output slot using one explicit private IPC root.
///
/// # Errors
/// Returns an invalid output key, reader admission or metadata error. A cold/stale cache is `None`.
#[cfg(unix)]
#[doc(hidden)]
pub fn ready_host_artifacts_for_generated_output_with_ipc_root(
    generated_output_key: &str,
    ipc_root: &std::path::Path,
) -> LeanResult<Option<LeanPreparedHostArtifacts>> {
    let selection = crate::host::lake::ShimOutputSelection::resolve(Some(generated_output_key))?;
    crate::host::lake::ready_artifacts(host_shim_export_signatures(), Some(ipc_root), &selection)
}

/// Capture ready bundled outputs under local read locks. This is not load authority.
///
/// # Errors
/// Refuses busy writers or invalid coordination state. No writer runs here.
#[cfg(unix)]
#[doc(hidden)]
pub fn ready_host_artifacts_locally() -> LeanResult<Option<LeanPreparedHostArtifacts>> {
    let selection = crate::host::lake::ShimOutputSelection::resolve(None)?;
    crate::host::lake::ready_artifacts_locally(host_shim_export_signatures(), &selection)
}

/// Prepare the default bundled output without waiting for writer admission.
///
/// # Errors
/// Returns busy admission, helper, build or snapshot errors.
#[doc(hidden)]
pub fn prepare_host_artifacts_without_lock_wait() -> LeanResult<LeanPreparedHostArtifacts> {
    let selection = crate::host::lake::ShimOutputSelection::resolve(None)?;
    let built = crate::host::lake::LakeProject::shim_capability_with_admission(
        host_shim_export_signatures(),
        &selection,
        true,
    )?;
    crate::host::lake::prepared_artifacts(&built, selection.key())
}

pub(super) fn reject_same_process_cargo_test_session_import() -> LeanResult<()> {
    if !same_process_cargo_test_import_guard_active() {
        return Ok(());
    }

    Err(lean_rs::__host_internals::host_resource_exhausted(format!(
        "lean-rs-host full-session imports are disabled under same-process cargo test; \
         run `cargo nextest run -p lean-rs-host ...` for process-per-test isolation, \
         or set {HOST_CARGO_TEST_IMPORT_OVERRIDE}=1 for an explicitly budgeted single-test debug run"
    )))
}

fn same_process_cargo_test_import_guard_active() -> bool {
    same_process_cargo_test_import_guard_active_from_facts(
        std::env::var(HOST_CARGO_TEST_IMPORT_OVERRIDE).ok().as_deref(),
        std::env::var_os("NEXTEST").is_some() || std::env::var_os("NEXTEST_RUN_ID").is_some(),
        is_probably_cargo_test_binary(),
    )
}

fn same_process_cargo_test_import_guard_active_from_facts(
    override_value: Option<&str>,
    nextest_present: bool,
    is_cargo_test_binary: bool,
) -> bool {
    if env_value_truthy(override_value) || nextest_present {
        return false;
    }
    is_cargo_test_binary
}

fn env_value_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| matches!(value, "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"))
}

fn is_probably_cargo_test_binary() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(parent) = exe.parent() else {
        return false;
    };

    // Cargo/libtest integration and unit test harnesses live under
    // `target/{profile}/deps`. Normal examples, profiling binaries, and worker
    // child processes do not.
    parent
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("deps"))
}

#[cfg(test)]
mod tests {
    use super::{
        LeanResult, generated_output_key_from_search_paths, same_process_cargo_test_import_guard_active_from_facts,
    };
    use std::path::PathBuf;

    #[test]
    fn cargo_test_import_guard_blocks_same_process_libtest_harnesses() {
        assert!(same_process_cargo_test_import_guard_active_from_facts(
            None, false, true,
        ));
    }

    #[test]
    fn cargo_test_import_guard_allows_nextest_override_and_non_test_binaries() {
        assert!(!same_process_cargo_test_import_guard_active_from_facts(
            None, true, true,
        ));
        assert!(!same_process_cargo_test_import_guard_active_from_facts(
            Some("1"),
            false,
            true,
        ));
        assert!(!same_process_cargo_test_import_guard_active_from_facts(
            None, false, false,
        ));
    }

    #[test]
    fn prepared_search_paths_select_one_generated_output_or_the_default() -> LeanResult<()> {
        let path = |package: &str, output: &str| PathBuf::from(format!("/sdk/{package}/.lake/{output}/lib/lean"));
        assert_eq!(
            generated_output_key_from_search_paths(&[
                path("interop", "lean-rs-generated/lean-4.36.0"),
                path("host", "lean-rs-generated/lean-4.36.0"),
            ])?,
            Some("lean-4.36.0".to_owned())
        );
        assert_eq!(
            generated_output_key_from_search_paths(&[path("interop", "build"), path("host", "build")])?,
            None
        );
        assert!(
            generated_output_key_from_search_paths(&[
                path("interop", "build"),
                path("host", "lean-rs-generated/lean-4.36.0"),
            ])
            .is_err()
        );
        assert!(
            generated_output_key_from_search_paths(&[
                path("interop", "lean-rs-generated/lean-4.36.0"),
                path("host", "lean-rs-generated/../bad"),
            ])
            .is_err()
        );
        assert!(
            generated_output_key_from_search_paths(&[
                path("interop", "lean-rs-generated/.bad"),
                path("host", "lean-rs-generated/.bad"),
            ])
            .is_err()
        );
        assert_eq!(
            generated_output_key_from_search_paths(&[
                PathBuf::from("/custom/interop/imports"),
                PathBuf::from("/custom/host/imports"),
            ])?,
            None
        );
        Ok(())
    }
}
