//! Lake project discovery helper.
//!
//! [`LakeProject`] resolves the on-disk layout Lake produces for a Lean
//! package: where the compiled `.dylib`/`.so` for a capability library
//! lives, and where the `.olean` files an imported module needs reside.
//! Both layouts are stable across the supported toolchain range
//! (Lean 4.29.x); paths are built by concatenation, not glob.
//!
//! The type is `pub(crate)` — `LeanHost` exposes the only operations
//! callers actually want (open the project, load a capability dylib).

use std::path::{Path, PathBuf};

use crate::error::{HostStage, LeanError, LeanResult};

/// A validated Lake project root.
///
/// Owned by [`crate::host::LeanHost`]; never escapes the `host` module.
pub(crate) struct LakeProject {
    root: PathBuf,
}

impl LakeProject {
    /// Bind a `LakeProject` to the given directory.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Load`] if the
    /// path does not exist or is not a directory. Diagnostic embeds the
    /// requested path.
    pub(crate) fn new(root: impl AsRef<Path>) -> LeanResult<Self> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(LeanError::host(
                HostStage::Load,
                format!(
                    "Lake project root '{}' does not exist or is not a directory",
                    root.display()
                ),
            ));
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// On-disk path to the compiled capability dylib for the
    /// `(package, lean_lib_name)` pair.
    ///
    /// Mirrors Lake's `.lake/build/lib/lib{escaped_package}_{lib_name}.{dylib,so}`
    /// layout. Lake escapes underscores in the package name by doubling
    /// them so the boundary between the package and the library name is
    /// unambiguous (verified against
    /// `fixtures/lean/.lake/build/lib/liblean__rs__fixture_LeanRsFixture.dylib`
    /// for `package="lean_rs_fixture"`, `lib_name="LeanRsFixture"`). The
    /// platform suffix selection mirrors
    /// `module::tests::fixture_dylib_path` (verified for macOS + Linux).
    pub(crate) fn capability_dylib(&self, package: &str, lib_name: &str) -> PathBuf {
        let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
        let escaped_package = package.replace('_', "__");
        self.root
            .join(".lake")
            .join("build")
            .join("lib")
            .join(format!("lib{escaped_package}_{lib_name}.{dylib_extension}"))
    }

    /// Search path the Lean side passes to `Lean.initSearchPath` so
    /// `Lean.importModules` can locate the `.olean` files Lake built for
    /// this project.
    pub(crate) fn olean_search_path(&self) -> PathBuf {
        self.root.join(".lake").join("build").join("lib").join("lean")
    }
}
