//! Guard: `lean-rs-host`'s bundled `lean-rs-interop-shims` must stay a verbatim
//! copy of the canonical package under `crates/lean-rs/`.
//!
//! The package is duplicated, not shared, because a published crate's
//! `Cargo.toml` `include` cannot reach outside its own directory, so each crate
//! must vendor its own self-contained copy (see
//! `docs/architecture/11-generic-interop-shims.md`). Duplication without a guard
//! drifts: `LeanRsInterop/Worker/Stream.lean` was once added to the canonical
//! copy alone. This test makes the "two copies, byte-identical" invariant
//! mechanically enforced instead of a comment nobody re-checks.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Collect every file under `root` (excluding `.lake/` build output) as a map
/// from root-relative path to byte contents.
fn collect_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
        for entry in entries {
            let entry = entry.expect("dir entry is readable");
            let path = entry.path();
            // `.lake/` is per-toolchain build output, not source — never part of parity.
            if path.file_name().is_some_and(|name| name == ".lake") {
                continue;
            }
            let file_type = entry.file_type().expect("file type is readable");
            if file_type.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(root).expect("entry lives under root").to_path_buf();
                let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                files.insert(rel, bytes);
            }
        }
    }
    files
}

#[test]
fn host_interop_shims_is_a_verbatim_copy_of_the_canonical_package() {
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

    let canonical_tree = collect_tree(&canonical);
    let host_tree = collect_tree(&host);

    let canonical_paths: Vec<_> = canonical_tree.keys().collect();
    let host_paths: Vec<_> = host_tree.keys().collect();
    assert_eq!(
        canonical_paths,
        host_paths,
        "the two lean-rs-interop-shims copies have different file sets;\n  canonical: {}\n  host:      {}\nsync the host copy from the canonical one (they must be byte-identical)",
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
        "the two lean-rs-interop-shims copies differ in: {mismatched:?};\nsync the host copy ({}) from the canonical one ({}) — they must be byte-identical",
        host.display(),
        canonical.display(),
    );
}
