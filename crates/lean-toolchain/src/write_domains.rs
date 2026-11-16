//! One bounded Lake workspace graph for cooperating artifact readers and writers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

pub(crate) const MAX_ROOTS: usize = 32;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn read_json(path: &Path) -> io::Result<Value> {
    // Opening a FIFO must not wait for a producer before we can reject it.
    // Validate the opened descriptor, preserving symlinks to regular metadata.
    #[cfg(unix)]
    let file = File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?);
    #[cfg(not(unix))]
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("Lake dependency metadata must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(invalid("Lake dependency metadata exceeds its byte bound"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn string<'a>(value: &'a Value, key: &str) -> io::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("Lake metadata requires {key}")))
}

fn relative(value: &str) -> io::Result<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid("Lake metadata path must remain inside its package"));
    }
    Ok(path)
}

fn package_name(value: &Value) -> io::Result<String> {
    let value = string(value, "name")?;
    let name = value
        .strip_prefix('«')
        .and_then(|name| name.strip_suffix('»'))
        .unwrap_or(value);
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '«', '»']) {
        return Err(invalid("unsupported Lake package name"));
    }
    Ok(name.to_owned())
}

fn manifest(path: &Path) -> io::Result<Value> {
    let value = read_json(path)?;
    let packages = packages(&value)?;
    match value.get("version") {
        Some(Value::String(version)) if version.starts_with("1.") => {}
        None if packages.is_empty() => {} // A proven zero-dependency generated payload has no graph to resolve.
        _ => return Err(invalid("unsupported Lake manifest schema")),
    }
    Ok(value)
}

fn packages(value: &Value) -> io::Result<&[Value]> {
    value
        .get("packages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("Lake packages must be an array"))
}

fn no_overrides(root: &Path, value: &Value) -> io::Result<()> {
    if value.get("lakeDir").is_some_and(|value| value != ".lake") {
        return Err(invalid(
            "nondefault Lake metadata directories require a supported runtime descriptor",
        ));
    }
    // Manifest lakeDir is stored metadata, not authority to redirect the
    // runtime's package-overrides path. The supported runtime uses .lake.
    let path = root.join(".lake/package-overrides.json");
    match read_json(&path) {
        Ok(overrides)
            if overrides.as_array().is_some_and(Vec::is_empty)
                || overrides.as_object().is_some_and(serde_json::Map::is_empty) =>
        {
            Ok(())
        }
        Ok(_) => Err(invalid(
            "Lake package overrides require an explicitly supported dependency graph",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

struct ArtifactGraph {
    roots: Vec<PathBuf>,
    git_roots: Vec<PathBuf>,
}

pub(crate) fn artifact_roots(root: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(artifact_graph(root)?.roots)
}

fn artifact_graph(root: &Path) -> io::Result<ArtifactGraph> {
    let root = crate::shared_writer::canonical_project_root(root)?;
    if root.try_exists()? && !root.metadata()?.is_dir() {
        return Err(invalid("artifact root is not a directory"));
    }
    let manifest_path = root.join("lake-manifest.json");
    if !manifest_path.try_exists()? {
        if root.join("lakefile.lean").try_exists()? || root.join("lakefile.toml").try_exists()? {
            return Err(invalid("Lake project has no dependency manifest"));
        }
        return Ok(ArtifactGraph {
            roots: vec![root],
            git_roots: Vec::new(),
        });
    }
    let primary = manifest(&manifest_path)?;
    no_overrides(&root, &primary)?;
    let packages_dir = primary.get("packagesDir").map_or(Ok(".lake/packages"), |value| {
        value.as_str().ok_or_else(|| invalid("invalid Lake packages directory"))
    })?;
    let packages_dir = root.join(relative(packages_dir)?);
    let mut entries = BTreeMap::new();
    let mut roots = BTreeSet::from([root.clone()]);
    let mut git_roots = Vec::new();
    for entry in packages(&primary)? {
        if entries.len() >= MAX_ROOTS - 1 {
            return Err(invalid("Lake dependency graph exceeds thirty-two roots"));
        }
        let name = package_name(entry)?;
        let package = match string(entry, "type")? {
            "path" => {
                if entry.get("copy").is_some_and(|copy| copy != false) {
                    return Err(invalid(
                        "copied Lake dependencies are outside the stable-domain contract",
                    ));
                }
                root.join(string(entry, "dir")?).canonicalize()?
            }
            "git" => {
                if primary.get("packagesDir").and_then(Value::as_str).is_none() {
                    return Err(invalid(
                        "Git dependencies require the manifest's explicit packages directory",
                    ));
                }
                if entry.get("subDir").is_some_and(|path| !path.is_null() && path != "") {
                    return Err(invalid("Lake Git subdirectories can replace a nested lock domain"));
                }
                let package = packages_dir.join(&name).canonicalize()?;
                git_roots.push(package.clone());
                package
            }
            _ => return Err(invalid("unsupported Lake dependency source")),
        };
        if !package.is_dir() {
            return Err(invalid("Lake dependency root is not a directory"));
        }
        let manifest_name = relative(string(entry, "manifestFile")?)?;
        let dependency_manifest = manifest(&package.join(manifest_name))?;
        no_overrides(&package, &dependency_manifest)?;
        if entries.insert(name, dependency_manifest).is_some() {
            return Err(invalid("duplicate Lake dependency name"));
        }
        roots.insert(package);
    }
    // Lake's primary manifest is already flattened. Its workspace map, rather
    // than a dependency's own .lake/packages directory, resolves every name.
    for dependency in entries.values() {
        for entry in packages(dependency)? {
            if !entries.contains_key(&package_name(entry)?) {
                return Err(invalid(
                    "Lake dependency is absent from the flattened workspace manifest",
                ));
            }
        }
    }
    validate_git_layout(&roots, &git_roots)?;
    Ok(ArtifactGraph {
        roots: roots.into_iter().collect(),
        git_roots,
    })
}

fn validate_git_layout(roots: &BTreeSet<PathBuf>, git_roots: &[PathBuf]) -> io::Result<()> {
    for project in roots {
        let lock = crate::shared_writer::project_lock_path(project)?;
        if git_roots.iter().any(|git| lock.starts_with(git)) {
            return Err(invalid("Lake Git cleanup could unlink a nested project lock"));
        }
    }
    Ok(())
}

pub(crate) fn workspace_roots(workspaces: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    if workspaces.is_empty() || workspaces.len() > MAX_ROOTS {
        return Err(invalid("artifact union requires one to thirty-two workspaces"));
    }
    let mut roots = BTreeSet::new();
    let mut git_roots = Vec::new();
    let mut seen = BTreeSet::new();
    for workspace in workspaces {
        let workspace = crate::shared_writer::canonical_project_root(workspace)?;
        if !seen.insert(workspace.clone()) {
            continue;
        }
        let graph = artifact_graph(&workspace)?;
        roots.extend(graph.roots);
        git_roots.extend(graph.git_roots);
        if roots.len() > MAX_ROOTS {
            return Err(invalid("Lake dependency graph exceeds thirty-two roots"));
        }
    }
    validate_git_layout(&roots, &git_roots)?;
    Ok(roots.into_iter().collect())
}

pub(crate) fn workspace_union(primary: &Path, prepared: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let mut workspaces = vec![primary.to_path_buf()];
    for workspace in prepared {
        if workspace != primary {
            workspaces.push(workspace.clone());
        }
    }
    workspace_roots(&workspaces)
}

#[cfg(test)]
mod tests;
