//! Lake project discovery helper.
//!
//! [`LakeProject`] resolves the on-disk layout Lake produces for a Lean
//! package: where the compiled `.dylib`/`.so` for a capability library
//! lives, where the `.olean` files an imported module needs reside, and
//! where the required `lean-rs-host-shims` Lake package's compiled
//! dylib lives so the host stack can load it alongside the user's
//! capability dylib. The layouts are stable across the supported
//! toolchain range (Lean 4.29.x); paths are built by concatenation
//! plus a small JSON read of `lake-manifest.json` for the shim
//! package's resolved directory.
//!
//! The type is `pub(crate)` — `LeanHost` exposes the only operations
//! callers actually want (open the project, load a capability dylib).

use std::path::{Path, PathBuf};

use lean_rs::error::LeanResult;

/// Lake package name the shim contract ships under. Constant on the
/// `lean-rs-host` side; consumers must use exactly this name in their
/// `require` line so the manifest lookup finds it.
pub(crate) const SHIM_PACKAGE_NAME: &str = "lean_rs_host_shims";
/// Lake `lean_lib` name inside the shim package. Constant on our side
/// because the shim package is ours; consumers don't re-declare it.
pub(crate) const SHIM_LIB_NAME: &str = "LeanRsHostShims";
/// Lake package name for the generic Lean/Rust interop shims used by
/// host progress callbacks.
pub(crate) const INTEROP_PACKAGE_NAME: &str = "lean_rs_interop_shims";
/// Lake `lean_lib` name inside the generic interop shim package.
pub(crate) const INTEROP_LIB_NAME: &str = "LeanRsInterop";

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

    /// Search path for the `lean-rs-host-shims` package's `.olean`
    /// files. Resolved via the same Lake manifest as
    /// [`Self::shim_dylib`]; a session that imports `LeanRsHostShims.*`
    /// needs this entry on the search path so the shim package's
    /// `.olean` files are reachable at runtime.
    ///
    /// # Errors
    ///
    /// Same as [`Self::shim_dylib`].
    pub(crate) fn shim_olean_search_path(&self) -> LeanResult<PathBuf> {
        let manifest_path = self.root.join("lake-manifest.json");
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' could not be read ({err})",
                manifest_path.display()
            ))
        })?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' is not valid JSON: {err}",
                manifest_path.display()
            ))
        })?;
        let package_dir = shim_package_dir_from_manifest(&self.root, &manifest, &manifest_path)?;
        Ok(package_dir.join(".lake").join("build").join("lib").join("lean"))
    }

    /// Search path for the generic `lean-rs-interop-shims` package's `.olean`
    /// files. Host shim modules import `LeanRsInterop.Callback`, so sessions
    /// that import `LeanRsHostShims.*` need this entry on the Lean search path.
    pub(crate) fn interop_olean_search_path(&self) -> LeanResult<PathBuf> {
        let manifest_path = self.root.join("lake-manifest.json");
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' could not be read ({err})",
                manifest_path.display()
            ))
        })?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' is not valid JSON: {err}",
                manifest_path.display()
            ))
        })?;
        let package_dir = package_dir_from_manifest(&self.root, &manifest, &manifest_path, INTEROP_PACKAGE_NAME)?;
        Ok(package_dir.join(".lake").join("build").join("lib").join("lean"))
    }

    /// Source roots passed to the source-range shim.
    ///
    /// Lean's declaration-range extension stores positions, not a stable
    /// absolute path. The host keeps Lake layout knowledge here and lets
    /// the Lean shim resolve a declaration's module against these roots
    /// when it needs a user-facing file label.
    pub(crate) fn source_roots(&self) -> LeanResult<Vec<PathBuf>> {
        let manifest_path = self.root.join("lake-manifest.json");
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' could not be read ({err})",
                manifest_path.display()
            ))
        })?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' is not valid JSON: {err}",
                manifest_path.display()
            ))
        })?;
        let shim_package_dir = shim_package_dir_from_manifest(&self.root, &manifest, &manifest_path)?;
        Ok(vec![self.root.clone(), shim_package_dir])
    }

    /// Resolve the on-disk dylib path for the required
    /// [`SHIM_PACKAGE_NAME`] Lake package by reading this project's
    /// `lake-manifest.json`. The consumer must have a `require` line
    /// pointing at the `lean-rs-host-shims` package (path or git);
    /// `lake build` then materialises the dylib at a location that
    /// depends on whether the require is path-backed or git-backed.
    ///
    /// For `type == "path"` requires Lake leaves the package in-place
    /// at the path the user supplied (relative to their lakefile);
    /// for `type == "git"` requires Lake clones into
    /// `<lake_root>/.lake/packages/<name>/`. Either way, after
    /// `lake build` runs, the dylib lives at
    /// `<package_dir>/.lake/build/lib/liblean__rs__host__shims_LeanRsHostShims.{dylib,so}`.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with
    /// [`lean_rs::LeanDiagnosticCode::ModuleInit`] if the manifest is
    /// missing (consumer never ran `lake update`), if no entry names
    /// the shim package (consumer forgot the `require` line), or if
    /// the manifest's JSON shape doesn't match Lake's documented
    /// format (Lake bumped manifest schema beyond supported range).
    pub(crate) fn shim_dylib(&self) -> LeanResult<PathBuf> {
        self.package_dylib(SHIM_PACKAGE_NAME, SHIM_LIB_NAME)
    }

    /// Resolve the on-disk dylib path for the generic interop shim package.
    ///
    /// Host progress shims import `LeanRsInterop.Callback`. Lake records that
    /// inherited dependency in the consumer's manifest, but it does not link
    /// the generic package shared library into the host shim dylib. The host
    /// loader opens this dylib globally before initializing host shims so the
    /// generated interop initializers resolve normally.
    pub(crate) fn interop_dylib(&self) -> LeanResult<PathBuf> {
        self.package_dylib(INTEROP_PACKAGE_NAME, INTEROP_LIB_NAME)
    }

    fn package_dylib(&self, package_name: &str, lib_name: &str) -> LeanResult<PathBuf> {
        let manifest_path = self.root.join("lake-manifest.json");
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' could not be read ({err}); the consumer must run `lake update` after \
                 adding the required lean-rs shim packages to their lakefile",
                manifest_path.display()
            ))
        })?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' is not valid JSON: {err}",
                manifest_path.display()
            ))
        })?;
        let package_dir = package_dir_from_manifest(&self.root, &manifest, &manifest_path, package_name)?;
        let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
        let lib_dir = package_dir.join(".lake").join("build").join("lib");
        let escaped_package = package_name.replace('_', "__");
        let new_style = lib_dir.join(format!("lib{escaped_package}_{lib_name}.{dylib_extension}"));
        let old_style = lib_dir.join(format!("lib{lib_name}.{dylib_extension}"));
        if new_style.is_file() {
            Ok(new_style)
        } else if old_style.is_file() {
            Ok(old_style)
        } else {
            Ok(new_style)
        }
    }
}

/// Walk Lake's `packages` array to find the entry whose `name` matches
/// [`SHIM_PACKAGE_NAME`] and resolve its on-disk directory.
fn shim_package_dir_from_manifest(
    lake_root: &Path,
    manifest: &serde_json::Value,
    manifest_path: &Path,
) -> LeanResult<PathBuf> {
    package_dir_from_manifest(lake_root, manifest, manifest_path, SHIM_PACKAGE_NAME)
}

/// Walk Lake's `packages` array to find the entry whose `name` matches
/// `package_name` and resolve its on-disk directory.
fn package_dir_from_manifest(
    lake_root: &Path,
    manifest: &serde_json::Value,
    manifest_path: &Path,
    package_name: &str,
) -> LeanResult<PathBuf> {
    let packages = manifest.get("packages").and_then(|p| p.as_array()).ok_or_else(|| {
        lean_rs::__host_internals::host_module_init(format!(
            "Lake manifest at '{}' has no `packages` array (unexpected manifest schema)",
            manifest_path.display()
        ))
    })?;
    let entry = packages
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(package_name))
        .ok_or_else(|| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake manifest at '{}' lists no `{package_name}` package; the consumer's lakefile \
                 must require the lean-rs shim packages and then run `lake update`",
                manifest_path.display()
            ))
        })?;
    let package_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match package_type {
        // Path-backed require: Lake stores the package in-place; the
        // `dir` field is relative to the consumer's lake_root.
        "path" => {
            let dir = entry.get("dir").and_then(|d| d.as_str()).ok_or_else(|| {
                lean_rs::__host_internals::host_module_init(format!(
                    "Lake manifest entry for `{package_name}` has type=\"path\" but no `dir` field"
                ))
            })?;
            Ok(lake_root.join(dir))
        }
        // Git-backed require: Lake clones into the project's
        // `packagesDir` (default `.lake/packages/<name>`).
        "git" => {
            let packages_dir = manifest
                .get("packagesDir")
                .and_then(|p| p.as_str())
                .unwrap_or(".lake/packages");
            Ok(lake_root.join(packages_dir).join(package_name))
        }
        other => Err(lean_rs::__host_internals::host_module_init(format!(
            "Lake manifest entry for `{package_name}` has unsupported require type '{other}' \
             (only `path` and `git` are supported today)"
        ))),
    }
}
