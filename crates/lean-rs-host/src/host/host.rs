//! `LeanHost` — entry point for the host-side capability API.
//!
//! A [`LeanHost`] binds a [`lean_rs::LeanRuntime`] borrow to a Lake project
//! on disk. From it, [`LeanHost::load_capabilities`] opens a compiled
//! capability dylib (e.g. `liblean__rs__fixture_LeanRsFixture.dylib`),
//! pre-resolves the capability's session symbol addresses, and returns
//! a [`LeanCapabilities`] that subsequent calls dispatch through without
//! per-call `dlsym`.
//!
//! See `docs/architecture/03-host-stack.md` for the full classification
//! and the host → capabilities → session lifetime cascade.

use core::fmt;
use std::path::Path;

use crate::host::capabilities::LeanCapabilities;
use crate::host::lake::LakeProject;
use lean_rs::LeanRuntime;
use lean_rs::error::LeanResult;
use lean_rs::module::LeanLibrary;

/// Entry point for hosting Lean capabilities from a Lake project.
///
/// Pairs a runtime borrow with a validated Lake project root. Cheap to
/// construct: only the project root's existence is checked; no dylib
/// loading happens until [`LeanHost::load_capabilities`] is called.
///
/// Neither [`Send`] nor [`Sync`]: inherited from the contained
/// `&'lean LeanRuntime`.
pub struct LeanHost<'lean> {
    runtime: &'lean LeanRuntime,
    project: LakeProject,
}

impl fmt::Debug for LeanHost<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeanHost").finish_non_exhaustive()
    }
}

impl<'lean> LeanHost<'lean> {
    /// Bind a host to the runtime and a Lake project root directory.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage
    /// [`lean_rs::HostStage::Load`] if `path` does not exist or is not a
    /// directory.
    pub fn from_lake_project(runtime: &'lean LeanRuntime, path: impl AsRef<Path>) -> LeanResult<Self> {
        let project = LakeProject::new(path)?;
        Ok(Self { runtime, project })
    }

    /// Load the compiled capability dylib for the named
    /// `(package, lib_name)` pair, initialize its root module, and
    /// pre-resolve the session symbol addresses.
    ///
    /// `package` is the Lake package name (e.g. `"lean_rs_fixture"`);
    /// `lib_name` is the Lake `lean_lib` declaration name and, by
    /// convention, also the root Lean module path
    /// (e.g. `"LeanRsFixture"`). For projects where these differ the
    /// `lib_name` argument is also the module that gets initialised.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with stage
    /// [`lean_rs::HostStage::Load`] if the dylib does not exist or fails
    /// to open, [`lean_rs::HostStage::Link`] if the initializer symbol or
    /// any of the twenty-seven mandatory session symbols is missing. The
    /// four optional `MetaM` symbols (`infer_type`, `whnf`,
    /// `heartbeat_burn`, `is_def_eq`) are looked up lazily and their
    /// absence does not fail loading; `run_meta` reports `Unsupported`
    /// at call time.
    pub fn load_capabilities<'h>(&'h self, package: &str, lib_name: &str) -> LeanResult<LeanCapabilities<'lean, 'h>> {
        let dylib_path = self.project.capability_dylib(package, lib_name);
        let library = LeanLibrary::open(self.runtime, &dylib_path)?;
        LeanCapabilities::new(self, library, package, lib_name)
    }

    /// The runtime borrow this host was constructed with.
    pub(crate) fn runtime(&self) -> &'lean LeanRuntime {
        self.runtime
    }

    /// The Lake project root, for capability code that needs to pass
    /// the path through to Lean (e.g., setting `Lean.searchPathRef`).
    pub(crate) fn project(&self) -> &LakeProject {
        &self.project
    }
}
