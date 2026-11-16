//! Canonical, bounded flattened Lake workspace domains.
#![allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use super::*;
use serde_json::json;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "lean-domain-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).expect("private domain fixture");
        Self(base.canonicalize().expect("canonical domain fixture"))
    }
    fn package(&self, name: &str) -> PathBuf {
        let root = self.0.join(name);
        fs::create_dir_all(&root).expect("package directory");
        write_manifest(&root, &[]);
        root
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _cleanup = fs::remove_dir_all(&self.0);
    }
}
fn write_manifest(root: &Path, packages: &[Value]) {
    fs::write(
        root.join("lake-manifest.json"),
        serde_json::to_vec(&json!({"version":"1.3.0", "packagesDir":".lake/packages", "packages":packages}))
            .expect("manifest JSON"),
    )
    .expect("write manifest");
}
fn git(name: &str) -> Value {
    json!({"name":name,"type":"git","subDir":null,"manifestFile":"lake-manifest.json"})
}
fn path(name: &str, root: &Path) -> Value {
    json!({"name":name,"type":"path","dir":root,"manifestFile":"lake-manifest.json"})
}

#[test]
fn flattened_dependencies_use_the_primary_workspace_map() {
    let f = Fixture::new();
    let a = f.package("A");
    let b = a.join(".lake/packages/B");
    let c = a.join(".lake/packages/C");
    fs::create_dir_all(&b).expect("B");
    fs::create_dir_all(&c).expect("C");
    write_manifest(&a, &[git("B"), git("C")]);
    write_manifest(&b, &[git("C")]);
    write_manifest(&c, &[]);
    assert!(!b.join(".lake/packages").exists());
    let roots = artifact_roots(&a).expect("flattened workspace");
    assert_eq!(roots.into_iter().collect::<BTreeSet<_>>(), BTreeSet::from([a, b, c]));
}

#[test]
fn external_path_and_symlink_aliases_share_one_canonical_domain() {
    let f = Fixture::new();
    let a = f.package("A");
    let b = f.package("B");
    #[cfg(unix)]
    {
        let alias = f.0.join("alias");
        std::os::unix::fs::symlink(&b, &alias).expect("path alias");
        write_manifest(&a, &[path("B", &b), path("Alias", &alias)]);
        assert_eq!(artifact_roots(&a).expect("canonical aliases"), vec![a, b]);
    }
}

#[test]
fn unsupported_or_missing_graph_metadata_never_degrades_to_primary_only() {
    let f = Fixture::new();
    let a = f.package("A");
    let b = f.package("B");
    write_manifest(&a, &[path("B", &b)]);
    fs::remove_file(b.join("lake-manifest.json")).expect("remove dependency manifest");
    assert_eq!(
        artifact_roots(&a).expect_err("missing child metadata").kind(),
        io::ErrorKind::NotFound
    );
    write_manifest(&b, &[git("Absent")]);
    assert!(
        artifact_roots(&a)
            .expect_err("missing flattened node")
            .to_string()
            .contains("flattened")
    );
    write_manifest(&a, &[]);
    fs::create_dir_all(a.join(".lake")).expect("lake metadata");
    fs::write(a.join(".lake/package-overrides.json"), b"{\"B\":\"elsewhere\"}").expect("override");
    assert!(
        artifact_roots(&a)
            .expect_err("unsupported overrides")
            .to_string()
            .contains("overrides")
    );
    fs::remove_file(a.join(".lake/package-overrides.json")).expect("remove override");
    fs::write(a.join("lake-manifest.json"), b"{\"version\":\"2.0.0\",\"packages\":[]}").expect("future schema");
    assert!(
        artifact_roots(&a)
            .expect_err("future schema")
            .to_string()
            .contains("schema")
    );
}

#[test]
fn git_cleanup_cannot_contain_any_other_project_lock() {
    let f = Fixture::new();
    let a = f.package("A");
    let b = a.join(".lake/packages/B");
    let nested = b.join("nested");
    fs::create_dir_all(&nested).expect("nested package");
    write_manifest(&b, &[]);
    write_manifest(&nested, &[]);
    write_manifest(&a, &[git("B"), path("Nested", &nested)]);
    assert!(
        artifact_roots(&a)
            .expect_err("nested lock is replaceable by Git cleanup")
            .to_string()
            .contains("unlink")
    );
    write_manifest(
        &a,
        &[json!({"name":"B","type":"git","subDir":"nested","manifestFile":"lake-manifest.json"})],
    );
    assert!(
        artifact_roots(&a)
            .expect_err("Git subdirectory")
            .to_string()
            .contains("subdirectories")
    );
}

#[test]
fn combined_workspaces_deduplicate_and_bound_all_domains() {
    let f = Fixture::new();
    let a = f.package("A");
    let b = f.package("B");
    write_manifest(&a, &[path("B", &b)]);
    assert_eq!(
        workspace_union(&a, &[b.clone(), a.clone()]).expect("combined roots"),
        vec![a.clone(), b.clone()]
    );
    let mut entries = Vec::new();
    for i in 0..31 {
        let name = format!("P{i}");
        entries.push(path(&name, &f.package(&name)));
    }
    write_manifest(&a, &entries);
    assert_eq!(artifact_roots(&a).expect("maximum graph").len(), 32);
    assert_eq!(artifact_roots(&b).expect("valid second graph"), vec![b.clone()]);
    assert!(
        workspace_union(&a, &[b])
            .expect_err("combined union exceeds thirty-two roots")
            .to_string()
            .contains("thirty-two")
    );
    entries.push(path("Extra", &f.package("Extra")));
    write_manifest(&a, &entries);
    assert!(
        artifact_roots(&a)
            .expect_err("too many domains")
            .to_string()
            .contains("thirty-two")
    );
}

#[test]
fn workspace_input_list_is_nonempty_and_bounded() {
    let f = Fixture::new();
    let a = f.package("A");
    assert!(
        workspace_roots(&[])
            .expect_err("empty input")
            .to_string()
            .contains("one to thirty-two")
    );
    assert_eq!(
        workspace_roots(&vec![a.clone(); 32]).expect("maximum input deduplicates"),
        vec![a.clone()]
    );
    assert!(
        workspace_roots(&vec![a; 33])
            .expect_err("oversized input")
            .to_string()
            .contains("one to thirty-two")
    );
}

#[test]
fn lake_projects_require_a_manifest_but_non_lake_inputs_have_one_domain() {
    let f = Fixture::new();
    let a = f.package("A");
    fs::remove_file(a.join("lake-manifest.json")).expect("remove manifest");
    assert_eq!(artifact_roots(&a).expect("non-Lake fixture"), vec![a.clone()]);
    fs::write(a.join("lakefile.toml"), b"name=\"a\"").expect("Lake marker");
    assert!(
        artifact_roots(&a)
            .expect_err("Lake project missing manifest")
            .to_string()
            .contains("no dependency manifest")
    );
}

#[test]
fn admission_rechecks_expansion_and_retains_every_new_lock() {
    use std::cell::Cell;
    let f = Fixture::new();
    let a = f.package("A");
    let b = f.package("B");
    let changed = Cell::new(false);
    // This probe runs during individual lock acquisition, after the initial
    // workspace snapshot, so the changed graph must be rechecked before admission.
    let cancelled = || {
        if !changed.replace(true) {
            write_manifest(&a, &[path("B", &b)]);
        }
        false
    };
    let (locks, roots) =
        crate::writer_lock::WriterLock::acquire_projects(&a, &[], std::time::Duration::from_secs(1), &cancelled)
            .expect("expanded admission");
    assert_eq!(roots, vec![a.clone(), b.clone()]);
    assert_eq!(
        locks.first().expect("primary loan").path(),
        crate::shared_writer::project_lock_path(&a).expect("primary path")
    );
    for root in [a, b] {
        let lock = File::open(crate::shared_writer::project_lock_path(&root).expect("lock path")).expect("contender");
        assert!(matches!(
            fs4::FileExt::try_lock(&lock),
            Err(fs4::TryLockError::WouldBlock)
        ));
    }
}
