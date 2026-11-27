//! Compiled-module lookup by the package-prefix rule of Lean's `findOLean`.
//!
//! This module validates Lean module names and picks the `.olean` path that
//! Lean would read. It does not read Lake's module graph; Lake owns that.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Typed diagnostics for compiled-module lookup.
#[derive(Debug)]
pub enum LeanModuleDiscoveryDiagnostic {
    InvalidModuleName {
        module: String,
        reason: String,
    },
    Io {
        path: PathBuf,
        message: &'static str,
        source: std::io::Error,
    },
}

impl fmt::Display for LeanModuleDiscoveryDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModuleName { module, reason } => {
                write!(f, "lean-toolchain: invalid Lean module name `{module}`: {reason}")
            }
            Self::Io { path, message, source } => {
                write!(f, "lean-toolchain: {message} at {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for LeanModuleDiscoveryDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidModuleName { .. } => None,
        }
    }
}

/// Locate a compiled module candidate using the package-prefix rule from Lean's `findOLean`.
///
/// Supply the worker's effective search paths in lookup order, including its
/// shim, inherited and builtin entries. For `A.B`, the first root containing a
/// directory `A/` or an entry `A.olean` selects `A/B.olean`, even if that leaf is missing.
/// Returns `None` if no root contains the prefix.
///
/// Module names are nonempty components separated by dots. Each component
/// starts with a Unicode alphabetic character or `_` and continues with
/// Unicode alphanumeric characters, underscores or ASCII apostrophes.
///
/// The metadata callback should follow symlinks as [`fs::metadata`] does. The
/// caller owns custody of the probes and must check the selected leaf under
/// that custody before using it. Lookup alone supplies no generation identity
/// or authority to import.
///
/// # Errors
///
/// Invalid module names fail before any probe. A missing path or non-directory
/// ancestor rules out that probe. Other metadata errors return a typed I/O diagnostic.
pub fn olean_candidate(
    ordered_roots: &[PathBuf],
    module: &str,
    mut metadata: impl FnMut(&Path) -> io::Result<fs::Metadata>,
) -> Result<Option<PathBuf>, LeanModuleDiscoveryDiagnostic> {
    validate_module_name(module)?;
    let prefix = module.split_once('.').map_or(module, |(first, _)| first);
    let mut probe = |path: &Path| match metadata(path) {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(None),
        Err(source) => Err(LeanModuleDiscoveryDiagnostic::Io {
            path: path.to_path_buf(),
            message: "could not inspect compiled module prefix",
            source,
        }),
    };
    for root in ordered_roots {
        let package = root.join(prefix);
        if probe(&package)?.is_some_and(|entry| entry.is_dir()) || probe(&package.with_extension("olean"))?.is_some() {
            return Ok(Some(module_to_dir(root, module).with_extension("olean")));
        }
    }
    Ok(None)
}

fn module_to_dir(project_root: &Path, module: &str) -> PathBuf {
    let mut path = project_root.to_path_buf();
    for part in module.split('.') {
        path.push(part);
    }
    path
}

fn validate_module_name(module: &str) -> Result<(), LeanModuleDiscoveryDiagnostic> {
    if module.is_empty() {
        return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
            module: module.to_owned(),
            reason: "module name is empty".to_owned(),
        });
    }
    for component in module.split('.') {
        if component.is_empty() {
            return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
                module: module.to_owned(),
                reason: "module name contains an empty component".to_owned(),
            });
        }
        let mut chars = component.chars();
        let Some(first) = chars.next() else {
            return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
                module: module.to_owned(),
                reason: "module name contains an empty component".to_owned(),
            });
        };
        if !(first == '_' || first.is_alphabetic()) {
            return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
                module: module.to_owned(),
                reason: "module components must begin with a letter or underscore".to_owned(),
            });
        }
        if chars.any(|ch| !(ch == '_' || ch == '\'' || ch.is_alphanumeric())) {
            return Err(LeanModuleDiscoveryDiagnostic::InvalidModuleName {
                module: module.to_owned(),
                reason: "module components may contain only letters, digits, underscores, or apostrophes".to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::{LeanModuleDiscoveryDiagnostic, olean_candidate};
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn olean_candidate_selects_first_package_even_when_leaf_is_missing() {
        let root = temp_project("olean-roots");
        let mut roots = [root.join("P"), root.join("D")];
        let project_leaf = roots[0].join("A/B.olean");
        let dependency_leaf = roots[1].join("A/B.olean");
        let resolve = |roots: &[PathBuf], module: &str| olean_candidate(roots, module, |p| fs::metadata(p)).unwrap();

        assert_eq!(resolve(&roots, "A.B"), None);
        write_file(&roots[0].join("A"), "a regular file does not own the prefix");
        write_file(&dependency_leaf, "present");
        assert_eq!(resolve(&roots, "A.B").as_ref(), Some(&dependency_leaf));

        fs::remove_file(roots[0].join("A")).unwrap();
        fs::create_dir(roots[0].join("A")).unwrap();
        assert_eq!(resolve(&roots, "A.B").as_ref(), Some(&project_leaf));
        assert!(!project_leaf.exists());
        roots.reverse();
        assert_eq!(resolve(&roots, "A.B").as_ref(), Some(&dependency_leaf));
    }

    #[test]
    fn olean_candidate_top_level_olean_owns_nested_names() {
        let root = temp_project("olean-prefix");
        let roots = [root.join("P"), root.join("D")];
        let top = roots[0].join("A.olean");
        write_file(&top, "present");
        write_file(&roots[1].join("A/B.olean"), "present");
        assert_eq!(olean_candidate(&roots, "A", |p| fs::metadata(p)).unwrap(), Some(top));
        assert_eq!(
            olean_candidate(&roots, "A.B", |p| fs::metadata(p)).unwrap(),
            Some(roots[0].join("A/B.olean"))
        );
        fs::remove_file(roots[0].join("A.olean")).unwrap();
        fs::create_dir(roots[0].join("A.olean")).unwrap();
        assert_eq!(
            olean_candidate(&roots, "A.B", |p| fs::metadata(p)).unwrap(),
            Some(roots[0].join("A/B.olean"))
        );
    }

    #[test]
    fn olean_candidate_skips_a_regular_file_search_entry() {
        let root = temp_project("olean-nondirectory");
        let roots = [root.join("regular-file"), root.join("D")];
        write_file(&roots[0], "cannot contain a package");
        let leaf = roots[1].join("A/B.olean");
        write_file(&leaf, "present");
        assert_eq!(olean_candidate(&roots, "A.B", |p| fs::metadata(p)).unwrap(), Some(leaf));
    }

    #[test]
    fn olean_candidate_stops_on_denied_prefix_metadata() {
        let root = temp_project("olean-denied");
        let roots = [root.join("P"), root.join("D")];
        write_file(&roots[1].join("A/B.olean"), "present");
        for denied in [roots[0].join("A"), roots[0].join("A.olean")] {
            let error = olean_candidate(&roots, "A.B", |p| {
                if p == denied {
                    return Err(std::io::ErrorKind::PermissionDenied.into());
                }
                fs::metadata(p)
            })
            .expect_err("cannot determine the first owning root");
            assert!(matches!(error, LeanModuleDiscoveryDiagnostic::Io { path, source, .. }
                if path == denied && source.kind() == std::io::ErrorKind::PermissionDenied));
        }
    }

    #[test]
    fn olean_candidate_validates_name_before_probing() {
        let roots = [PathBuf::from("unused")];
        let error = olean_candidate(&roots, "A..B", |_| panic!("unexpected filesystem probe")).unwrap_err();
        assert!(matches!(error, LeanModuleDiscoveryDiagnostic::InvalidModuleName { module, .. } if module == "A..B"));
    }

    fn temp_project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lean-toolchain-modules-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
}
