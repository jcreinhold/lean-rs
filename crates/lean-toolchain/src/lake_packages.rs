//! The Lake package closure of a project, as its `lake-manifest.json` records it.
//!
//! The build cache key hashes the sources of every package in this closure, so
//! an edit to a dependency's sources forces a rebuild. Lake flattens the closure
//! into the primary manifest, which therefore names every package.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn read_json(path: &Path) -> io::Result<Value> {
    // Opening a FIFO must not wait for a producer before we can reject it.
    // Check the opened descriptor, so symlinks to regular files still work.
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
        None if packages.is_empty() => {} // A generated payload with no dependencies has no graph to resolve.
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

// Overrides and a moved Lake directory relocate packages the manifest names,
// so the closure read from the manifest would miss their sources.
fn no_overrides(root: &Path, value: &Value) -> io::Result<()> {
    if value.get("lakeDir").is_some_and(|value| value != ".lake") {
        return Err(invalid("nondefault Lake metadata directories are not supported"));
    }
    let path = root.join(".lake/package-overrides.json");
    match read_json(&path) {
        Ok(overrides)
            if overrides.as_array().is_some_and(Vec::is_empty)
                || overrides.as_object().is_some_and(serde_json::Map::is_empty) =>
        {
            Ok(())
        }
        Ok(_) => Err(invalid("Lake package overrides are not supported")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The canonical roots of `root` and of every package in its Lake closure, sorted.
///
/// A directory with neither a manifest nor a lakefile is its own closure. An
/// unknown manifest schema, dependency source or package override is an
/// error, so the cache key never leaves out a package's sources.
pub(crate) fn package_roots(root: &Path) -> io::Result<Vec<PathBuf>> {
    let root = root.canonicalize()?;
    let manifest_path = root.join("lake-manifest.json");
    if !manifest_path.try_exists()? {
        if root.join("lakefile.lean").try_exists()? || root.join("lakefile.toml").try_exists()? {
            return Err(invalid("Lake project has no dependency manifest"));
        }
        return Ok(vec![root]);
    }
    let primary = manifest(&manifest_path)?;
    no_overrides(&root, &primary)?;
    let packages_dir = primary.get("packagesDir").map_or(Ok(".lake/packages"), |value| {
        value.as_str().ok_or_else(|| invalid("invalid Lake packages directory"))
    })?;
    let packages_dir = root.join(relative(packages_dir)?);
    let mut entries = BTreeMap::new();
    let mut roots = BTreeSet::from([root.clone()]);
    for entry in packages(&primary)? {
        let name = package_name(entry)?;
        let package = match string(entry, "type")? {
            "path" => root.join(string(entry, "dir")?).canonicalize()?,
            "git" => {
                // Lake clones the repository to `<packagesDir>/<name>`; a
                // package in a subdirectory records it as `subDir`.
                let mut package = packages_dir.join(&name);
                if let Some(sub_dir) = entry
                    .get("subDir")
                    .and_then(Value::as_str)
                    .filter(|dir| !dir.is_empty())
                {
                    package.push(relative(sub_dir)?);
                }
                package.canonicalize()?
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
    // The primary manifest is already flattened: every dependency of a
    // dependency must appear in it, not only in that dependency's manifest.
    for dependency in entries.values() {
        for entry in packages(dependency)? {
            if !entries.contains_key(&package_name(entry)?) {
                return Err(invalid(
                    "Lake dependency is absent from the flattened workspace manifest",
                ));
            }
        }
    }
    Ok(roots.into_iter().collect())
}

#[cfg(test)]
mod tests;
