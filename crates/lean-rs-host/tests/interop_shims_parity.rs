//! Guard: `lean-rs-host`'s bundled `lean-rs-interop-shims` payload must stay a
//! verbatim copy of the canonical package payload under `crates/lean-rs/`.
//!
//! The host vendors its own copy because a published crate's `Cargo.toml`
//! `include` cannot reach outside its own directory. The repository's
//! fixtures and examples build the canonical copy. Two copies drift without a
//! guard, so this test checks that they hold the same files with the same
//! bytes.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Collect every runtime payload file under `root` as a map from root-relative
/// path to byte contents.
fn collect_payload_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
        for entry in entries {
            let entry = entry.expect("dir entry is readable");
            let path = entry.path();
            let rel = path.strip_prefix(root).expect("entry lives under root").to_path_buf();
            // Lake's build output is not payload.
            if rel.starts_with(".lake") {
                continue;
            }
            let file_type = entry.file_type().expect("file type is readable");
            if file_type.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                files.insert(rel, bytes);
            }
        }
    }
    files
}

#[test]
fn host_interop_shims_payload_is_a_verbatim_copy_of_the_canonical_package_payload() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let host = manifest_dir.join("shims").join("lean-rs-interop-shims");

    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    let canonical = workspace
        .join("crates")
        .join("lean-rs")
        .join("shims")
        .join("lean-rs-interop-shims");

    // In an isolated published-crate checkout the sibling crate is absent; the
    // invariant only has meaning (and only needs checking) in the workspace.
    if !canonical.is_dir() {
        eprintln!(
            "skipping: canonical copy {} not present (isolated checkout)",
            canonical.display()
        );
        return;
    }

    let canonical_tree = collect_payload_tree(&canonical);
    let host_tree = collect_payload_tree(&host);

    let canonical_paths: Vec<_> = canonical_tree.keys().collect();
    let host_paths: Vec<_> = host_tree.keys().collect();
    assert_eq!(
        canonical_paths,
        host_paths,
        "the two lean-rs-interop-shims payloads have different file sets;\n  canonical: {}\n  host:      {}\nsync the host payload from the canonical one (payload files must be byte-identical)",
        canonical.display(),
        host.display(),
    );

    let mismatched: Vec<_> = canonical_tree
        .iter()
        .filter(|(rel, bytes)| host_tree.get(*rel) != Some(*bytes))
        .map(|(rel, _)| rel.display().to_string())
        .collect();
    assert!(
        mismatched.is_empty(),
        "the two lean-rs-interop-shims payloads differ in: {mismatched:?};\nsync the host payload ({}) from the canonical one ({}) — payload files must be byte-identical",
        host.display(),
        canonical.display(),
    );
}
