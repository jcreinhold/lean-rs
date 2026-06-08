//! Lake project discovery helper.
//!
//! [`LakeProject`] resolves the on-disk layout Lake produces for a Lean
//! package: where the compiled `.dylib`/`.so` for a capability library
//! lives, where the `.olean` files an imported module needs reside, and
//! where the bundled `lean-rs-host-shims` and `lean-rs-interop-shims`
//! packages compile their dylibs so the host stack can load them
//! alongside the user's capability dylib. The layouts are stable
//! across the supported toolchain range; paths are built
//! by concatenation and the bundled shims are built on demand through
//! `lean-toolchain`.
//!
//! The type is `pub(crate)`—`LeanHost` exposes the only operations
//! callers actually want (open the project, load a capability dylib).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lean_rs::error::LeanResult;
use lean_toolchain::{BuiltLeanCapability, LeanExportSignature};
use serde_json::Value;

/// Lake package name the host shim contract ships under.
pub(crate) const SHIM_PACKAGE_NAME: &str = "lean_rs_host_shims";
/// Lake `lean_lib` name inside the shim package. Constant on our side
/// because the shim package is ours; consumers don't re-declare it.
pub(crate) const SHIM_LIB_NAME: &str = "LeanRsHostShims";
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
        let root = fs::canonicalize(root).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake project root '{}' could not be canonicalized: {err}",
                root.display()
            ))
        })?;
        Ok(Self { root })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
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
    /// part of the supported window (see `docs/version-matrix.md`);
    /// this method returns
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
        project_olean_search_path(&self.root)
    }

    /// Search paths the Lean side passes to `Lean.initSearchPath` so
    /// `Lean.importModules` can locate the `.olean` files Lake built for
    /// this project and every package recorded in `lake-manifest.json`.
    ///
    /// Missing or malformed manifest data degrades to the project-local
    /// entry, matching the layout used by dependency-free Lake projects.
    pub(crate) fn olean_search_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.olean_search_path()];
        let manifest_path = self.root.join("lake-manifest.json");
        let Ok(manifest_bytes) = fs::read(manifest_path) else {
            return paths;
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&manifest_bytes) else {
            return paths;
        };
        let packages_dir = manifest
            .get("packagesDir")
            .and_then(Value::as_str)
            .unwrap_or(".lake/packages");
        let Some(packages) = manifest.get("packages").and_then(Value::as_array) else {
            return paths;
        };

        for package in packages {
            if let Some(package_root) = manifest_package_root(&self.root, packages_dir, package) {
                paths.push(project_olean_search_path(&package_root));
            }
        }
        paths
    }

    /// Search path for the bundled `lean-rs-host-shims` package's `.olean`
    /// files. A session that imports `LeanRsHostShims.*` needs this entry on
    /// the search path so the shim package's `.olean` files are reachable at
    /// runtime.
    ///
    /// # Errors
    ///
    /// Returns a module-initialization error if the bundled shim package
    /// cannot be built.
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

    /// Build the bundled host-shim target and emit a manifest carrying
    /// trusted export signatures.
    pub(crate) fn shim_capability(export_signatures: Vec<LeanExportSignature>) -> LeanResult<BuiltLeanCapability> {
        build_bundled_capability(&bundled_host_shims_root(), SHIM_LIB_NAME, export_signatures)
    }
}

fn project_olean_search_path(project_root: &Path) -> PathBuf {
    project_root.join(".lake").join("build").join("lib").join("lean")
}

fn manifest_package_root(project_root: &Path, packages_dir: &str, package: &Value) -> Option<PathBuf> {
    let package_name = package
        .get("name")
        .and_then(Value::as_str)
        .map(normalize_lake_identifier)?;
    if package.get("type").and_then(Value::as_str) == Some("path")
        && let Some(dir) = package.get("dir").and_then(Value::as_str)
    {
        return Some(project_root.join(dir));
    }
    Some(project_root.join(packages_dir).join(package_name))
}

fn normalize_lake_identifier(raw: &str) -> String {
    raw.trim()
        .trim_matches('`')
        .trim_matches('«')
        .trim_matches('»')
        .trim_matches('"')
        .trim()
        .to_owned()
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

fn build_bundled_capability(
    project_root: &Path,
    target_name: &str,
    export_signatures: Vec<LeanExportSignature>,
) -> LeanResult<BuiltLeanCapability> {
    let _guard = BUNDLED_SHIM_BUILD_LOCK.lock().map_err(|_| {
        lean_rs::__host_internals::host_module_init("bundled lean-rs shim build lock is poisoned".to_owned())
    })?;
    let mut builder = lean_toolchain::CargoLeanCapability::new(project_root, target_name)
        .package(SHIM_PACKAGE_NAME)
        .module(SHIM_LIB_NAME);
    for signature in export_signatures {
        builder = builder.export_signature(signature);
    }
    builder.build_quiet().map_err(|diagnostic| {
        lean_rs::__host_internals::host_module_init(format!(
            "could not build bundled lean-rs shim target `{target_name}` under {}: {diagnostic}",
            project_root.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TempProject {
        root: PathBuf,
    }

    impl TempProject {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("lean-rs-lake-{name}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&root).expect("create temp Lake root");
            Self { root }
        }

        fn project(&self) -> LakeProject {
            LakeProject::new(&self.root).expect("temp project opens")
        }

        fn write_manifest(&self, contents: &str) {
            fs::write(self.root.join("lake-manifest.json"), contents).expect("write manifest");
        }

        fn canonical_root(&self) -> PathBuf {
            fs::canonicalize(&self.root).expect("canonicalize temp Lake root")
        }

        fn own_olean_path(&self) -> PathBuf {
            self.canonical_root()
                .join(".lake")
                .join("build")
                .join("lib")
                .join("lean")
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.root));
        }
    }

    #[test]
    fn olean_search_paths_degrades_without_manifest() {
        let project = TempProject::new("no-manifest");

        assert_eq!(project.project().olean_search_paths(), vec![project.own_olean_path()]);
    }

    #[test]
    fn olean_search_paths_accepts_empty_package_manifest() {
        let project = TempProject::new("empty-manifest");
        project.write_manifest(r#"{"version":"1.2.0","packagesDir":".lake/packages","packages":[]}"#);

        assert_eq!(project.project().olean_search_paths(), vec![project.own_olean_path()]);
    }

    #[test]
    fn olean_search_paths_uses_manifest_package_entries() {
        let project = TempProject::new("packages");
        project.write_manifest(
            r#"{
                "version":"1.2.0",
                "packagesDir":"custom-packages",
                "packages":[
                    {"type":"git","name":"«doc-gen4»"},
                    {"type":"path","name":"dep","dir":"../dep"},
                    {"type":"git"},
                    "not a package"
                ]
            }"#,
        );

        assert_eq!(
            project.project().olean_search_paths(),
            vec![
                project.own_olean_path(),
                project
                    .canonical_root()
                    .join("custom-packages")
                    .join("doc-gen4")
                    .join(".lake")
                    .join("build")
                    .join("lib")
                    .join("lean"),
                project
                    .canonical_root()
                    .join("../dep")
                    .join(".lake")
                    .join("build")
                    .join("lib")
                    .join("lean"),
            ]
        );
    }

    #[test]
    fn olean_search_paths_degrades_on_malformed_manifest() {
        let project = TempProject::new("malformed-manifest");
        project.write_manifest("{not json");

        assert_eq!(project.project().olean_search_paths(), vec![project.own_olean_path()]);
    }
}
