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

use lean_rs::error::LeanResult;
use lean_rs::module::{LeanBuiltCapability, LeanCapability, LeanLibrary};

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
    _user_library: Option<LeanLibrary<'lean>>,
    /// Manifest-backed bundled host shim capability. Session construction
    /// resolves typed bindings from this checked surface.
    shim_capability: LeanCapability<'lean>,
}

impl fmt::Debug for LeanCapabilities<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeanCapabilities").finish_non_exhaustive()
    }
}

impl<'lean, 'h> LeanCapabilities<'lean, 'h> {
    /// Build a [`LeanCapabilities`] from an opened user-capability
    /// library, opening the generic interop and host shim dylibs alongside it.
    ///
    /// Initializes the root module of the user's dylib (idempotent
    /// through Lean's `_G_initialized` short-circuit), opens and
    /// initializes the generic interop and host shim dylibs built from the
    /// crate-owned shim sources, and resolves the
    /// manifest-backed shim capability. Session construction resolves 28
    /// mandatory and 9 optional checked typed bindings from that capability.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage
    /// [`lean_rs::HostStage::Load`] /
    /// [`lean_rs::LeanDiagnosticCode::ModuleInit`] if the bundled shim dylibs
    /// cannot be built or located. Returns [`lean_rs::HostStage::Link`] if the
    /// initializer or any of the 28 **mandatory** symbols is missing from the
    /// shim dylib. Missing optional meta-service symbols never fail capability
    /// load.
    pub(crate) fn new(
        host: &'h LeanHost<'lean>,
        user_library: LeanLibrary<'lean>,
        package: &str,
        lib_name: &str,
    ) -> LeanResult<Self> {
        let shim_capability = load_shim_capability(host)?;

        // Now the user dylib. It does not need to depend on the host shims;
        // ad-hoc user exports still resolve from this library.
        let _user_module = user_library.initialize_module(package, lib_name)?;

        Ok(Self {
            host,
            _user_library: Some(user_library),
            shim_capability,
        })
    }

    /// Build a [`LeanCapabilities`] backed only by the bundled interop and
    /// host shim dylibs.
    ///
    /// Sessions opened from this value can use every shim-backed session
    /// operation without loading a user capability dylib.
    pub(crate) fn new_shims_only(host: &'h LeanHost<'lean>) -> LeanResult<Self> {
        let shim_capability = load_shim_capability(host)?;
        Ok(Self {
            host,
            _user_library: None,
            shim_capability,
        })
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
        imports: &[&str],
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
        imports: &[&str],
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
        imports: &[&str],
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
        imports: &[&str],
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

    /// Manifest-backed bundled host shim capability.
    pub(crate) fn shim_capability(&self) -> &LeanCapability<'lean> {
        &self.shim_capability
    }
}

fn load_shim_capability<'lean>(host: &LeanHost<'lean>) -> LeanResult<LeanCapability<'lean>> {
    let built = crate::host::lake::LakeProject::shim_capability(host_shim_export_signatures())?;
    LeanCapability::from_build_manifest(
        host.runtime(),
        LeanBuiltCapability::manifest_path(built.manifest_path()),
    )
}

fn reject_same_process_cargo_test_session_import() -> LeanResult<()> {
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
    use super::same_process_cargo_test_import_guard_active_from_facts;

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
}
