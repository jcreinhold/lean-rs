//! Build-selected files for the packaged ShipLeanDemo worker.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub struct Deployment {
    pub project: PathBuf,
    pub sysroot: PathBuf,
    pub shim_manifest: PathBuf,
    pub shim_paths: Vec<PathBuf>,
    pub files: BTreeMap<PathBuf, String>,
}

pub fn digest(path: &Path) -> io::Result<String> {
    if path.canonicalize()? != path || !path.is_file() {
        return Err(io::Error::other("expected canonical regular deployment file"));
    }
    Ok(Sha256::digest(fs::read(path)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

impl Deployment {
    pub fn inventory(&self) -> io::Result<BTreeMap<PathBuf, String>> {
        let mut files = BTreeMap::new();
        for path in [
            &self.shim_manifest,
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("lean/lake-manifest.json")
                .as_path(),
        ] {
            files.insert(path.to_owned(), digest(path)?);
        }
        let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&self.shim_manifest)?)?;
        let images = std::iter::once(&manifest["primary_dylib"]).chain(
            manifest["dependencies"]
                .as_array()
                .ok_or_else(|| io::Error::other("invalid dependencies"))?
                .iter()
                .map(|d| &d["dylib_path"]),
        );
        for image in images {
            let p = PathBuf::from(image.as_str().ok_or_else(|| io::Error::other("invalid image"))?);
            files.insert(p.clone(), digest(&p)?);
        }
        let output = self.project.join(".lake/build/lib/lean");
        for root in std::iter::once(&output).chain(&self.shim_paths) {
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == "Init" || name.starts_with("Init.") {
                    return Err(io::Error::other("builtin Init shadow"));
                }
                if name == "ShipLeanDemo" || name.starts_with("ShipLeanDemo.") {
                    if root != &output || entry.file_type()?.is_dir() {
                        return Err(io::Error::other("unexpected module location"));
                    }
                    files.insert(entry.path(), digest(&entry.path())?);
                }
            }
        }
        if !files.contains_key(&output.join("ShipLeanDemo.olean")) {
            return Err(io::Error::other("missing packaged module"));
        }
        Ok(files)
    }
}

pub fn matches_demo(manifest: &serde_json::Value, image: &Path) -> bool {
    manifest["package"] == "ship_lean_demo"
        && manifest["module"] == "ShipLeanDemo"
        && manifest["primary_dylib"].as_str().map(Path::new) == Some(image)
        && manifest["dependencies"].as_array().is_some_and(Vec::is_empty)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_substituted_image_initializer_or_dependencies() {
        let image = Path::new("/selected/ShipLeanDemo");
        let selected = serde_json::json!({"package":"ship_lean_demo","module":"ShipLeanDemo","primary_dylib":image,"dependencies":[]});
        assert!(matches_demo(&selected, image));
        for (key, value) in [
            ("primary_dylib", serde_json::json!("/other")),
            ("package", serde_json::json!("other")),
            ("module", serde_json::json!("Other")),
            ("dependencies", serde_json::json!([{"dylib_path":"/other"}])),
        ] {
            let mut altered = selected.clone();
            altered[key] = value;
            assert!(!matches_demo(&altered, image));
        }
    }
}
