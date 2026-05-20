//! Lake project discovery helper.
//!
//! [`LakeProject`] resolves the on-disk layout Lake produces for a Lean
//! package: where the compiled `.dylib`/`.so` for a capability library
//! lives, where the `.olean` files an imported module needs reside, and
//! where the bundled `lean-rs-host-shims` and `lean-rs-interop-shims`
//! packages compile their dylibs so the host stack can load them
//! alongside the user's capability dylib. The layouts are stable
//! across the supported toolchain range (Lean 4.29.x); paths are built
//! by concatenation and the bundled shims are built on demand through
//! `lean-toolchain`.
//!
//! The type is `pub(crate)` — `LeanHost` exposes the only operations
//! callers actually want (open the project, load a capability dylib).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lean_rs::error::LeanResult;

/// Lake package name the host shim contract ships under.
pub(crate) const SHIM_PACKAGE_NAME: &str = "lean_rs_host_shims";
/// Lake `lean_lib` name inside the shim package. Constant on our side
/// because the shim package is ours; consumers don't re-declare it.
pub(crate) const SHIM_LIB_NAME: &str = "LeanRsHostShims";
/// Lake package name for the generic Lean/Rust interop shims used by
/// host progress callbacks.
pub(crate) const INTEROP_PACKAGE_NAME: &str = "lean_rs_interop_shims";
/// Lake `lean_lib` name inside the generic interop shim package.
pub(crate) const INTEROP_LIB_NAME: &str = "LeanRsInterop";

const BUNDLED_SHIMS_DIR: &str = "shims";
const BUNDLED_HOST_SHIMS_DIR: &str = "lean-rs-host-shims";
const BUNDLED_INTEROP_SHIMS_DIR: &str = "lean-rs-interop-shims";

static BUNDLED_SHIM_BUILD_LOCK: Mutex<()> = Mutex::new(());

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
    /// Returns [`lean_rs::LeanError::Host`] with code
    /// [`lean_rs::LeanDiagnosticCode::ModuleInit`] if the path does not
    /// exist or is not a directory. Diagnostic embeds the requested
    /// path.
    pub(crate) fn new(root: impl AsRef<Path>) -> LeanResult<Self> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(lean_rs::__host_internals::host_module_init(format!(
                "Lake project root '{}' does not exist or is not a directory",
                root.display()
            )));
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// On-disk path to the compiled capability dylib for the
    /// `(package, lean_lib_name)` pair.
    ///
    /// The on-disk path Lake materialises the consumer's `lean_lib` to,
    /// resolved by probing both supported naming conventions.
    ///
    /// Lake's shared-library filename changed between Lean 4.26 and 4.27:
    /// older versions emit `.lake/build/lib/lib{lib_name}.{dylib,so}` (just
    /// the library name); 4.27+ emit
    /// `.lake/build/lib/lib{escaped_package}_{lib_name}.{dylib,so}` where
    /// `escaped_package` doubles every underscore. Both conventions are
    /// part of the supported window (see
    /// [`lean_rs_sys::SUPPORTED_TOOLCHAINS`]); this method returns
    /// whichever candidate exists so the Rust loader is naming-convention-
    /// agnostic. Returns the new-style path as a fallback for diagnostics
    /// when neither candidate exists on disk; the caller surfaces the
    /// failure as an [`crate::error::ModuleInit`] error.
    pub(crate) fn capability_dylib(&self, package: &str, lib_name: &str) -> PathBuf {
        let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
        let lib_dir = self.root.join(".lake").join("build").join("lib");
        let escaped_package = package.replace('_', "__");
        let new_style = lib_dir.join(format!("lib{escaped_package}_{lib_name}.{dylib_extension}"));
        let old_style = lib_dir.join(format!("lib{lib_name}.{dylib_extension}"));
        if new_style.is_file() {
            new_style
        } else if old_style.is_file() {
            old_style
        } else {
            new_style
        }
    }

    /// Search path the Lean side passes to `Lean.initSearchPath` so
    /// `Lean.importModules` can locate the `.olean` files Lake built for
    /// this project.
    pub(crate) fn olean_search_path(&self) -> PathBuf {
        self.root.join(".lake").join("build").join("lib").join("lean")
    }

    /// Search path for the bundled `lean-rs-host-shims` package's `.olean`
    /// files. A session that imports `LeanRsHostShims.*` needs this entry on
    /// the search path so the shim package's `.olean` files are reachable at
    /// runtime.
    ///
    /// # Errors
    ///
    /// Same as [`Self::shim_dylib`].
    pub(crate) fn shim_olean_search_path() -> LeanResult<PathBuf> {
        let package_dir = bundled_host_shims_root();
        drop(build_bundled_target(&package_dir, SHIM_LIB_NAME)?);
        Ok(package_dir.join(".lake").join("build").join("lib").join("lean"))
    }

    /// Search path for the generic `lean-rs-interop-shims` package's `.olean`
    /// files. Host shim modules import `LeanRsInterop.Callback`, so sessions
    /// that import `LeanRsHostShims.*` need this entry on the Lean search path.
    pub(crate) fn interop_olean_search_path() -> LeanResult<PathBuf> {
        let package_dir = bundled_interop_shims_root();
        drop(build_bundled_target(&package_dir, INTEROP_LIB_NAME)?);
        Ok(package_dir.join(".lake").join("build").join("lib").join("lean"))
    }

    /// Source roots passed to the source-range shim.
    ///
    /// Lean's declaration-range extension stores positions, not a stable
    /// absolute path. The host keeps Lake layout knowledge here and lets
    /// the Lean shim resolve a declaration's module against these roots
    /// when it needs a user-facing file label.
    pub(crate) fn source_roots(&self) -> LeanResult<Vec<PathBuf>> {
        Ok(vec![self.root.clone(), bundled_host_shims_root()])
    }

    /// Resolve the bundled host-shim dylib, building it on demand if needed.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with
    /// [`lean_rs::LeanDiagnosticCode::ModuleInit`] if Lake cannot build the
    /// crate-owned shim package or the expected dylib cannot be resolved.
    pub(crate) fn shim_dylib() -> LeanResult<PathBuf> {
        build_bundled_target(&bundled_host_shims_root(), SHIM_LIB_NAME)
    }

    /// Resolve the bundled generic interop shim dylib, building it on demand if needed.
    ///
    /// Host progress shims import `LeanRsInterop.Callback`. The host loader
    /// opens this dylib globally before initializing host shims so the
    /// generated interop initializers resolve normally.
    pub(crate) fn interop_dylib() -> LeanResult<PathBuf> {
        build_bundled_target(&bundled_interop_shims_root(), INTEROP_LIB_NAME)
    }
}

fn bundled_host_shims_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(BUNDLED_SHIMS_DIR)
        .join(BUNDLED_HOST_SHIMS_DIR)
}

fn bundled_interop_shims_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(BUNDLED_SHIMS_DIR)
        .join(BUNDLED_INTEROP_SHIMS_DIR)
}

fn build_bundled_target(project_root: &Path, target_name: &str) -> LeanResult<PathBuf> {
    let _guard = BUNDLED_SHIM_BUILD_LOCK.lock().map_err(|_| {
        lean_rs::__host_internals::host_module_init("bundled lean-rs shim build lock is poisoned".to_owned())
    })?;
    lean_toolchain::build_lake_target_quiet(project_root, target_name).map_err(|diagnostic| {
        lean_rs::__host_internals::host_module_init(format!(
            "could not build bundled lean-rs shim target `{target_name}` under {}: {diagnostic}",
            project_root.display()
        ))
    })
}
