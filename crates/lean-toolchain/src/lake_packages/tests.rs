//! The Lake package closure read from a flattened manifest.

use super::*;
use serde_json::json;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "lean-packages-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base)?;
        Ok(Self(base.canonicalize()?))
    }
    fn package(&self, name: &str) -> io::Result<PathBuf> {
        let root = self.0.join(name);
        fs::create_dir_all(&root)?;
        write_manifest(&root, &[])?;
        Ok(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _cleanup = fs::remove_dir_all(&self.0);
    }
}
fn write_manifest(root: &Path, packages: &[Value]) -> io::Result<()> {
    let manifest = json!({"version":"1.3.0", "packagesDir":".lake/packages", "packages":packages});
    fs::write(root.join("lake-manifest.json"), serde_json::to_vec(&manifest)?)
}
fn git(name: &str) -> Value {
    json!({"name":name,"type":"git","subDir":null,"manifestFile":"lake-manifest.json"})
}
fn path(name: &str, root: &Path) -> Value {
    json!({"name":name,"type":"path","dir":root,"manifestFile":"lake-manifest.json"})
}
fn refused(root: &Path) -> Result<io::Error, String> {
    match package_roots(root) {
        Ok(roots) => Err(format!("expected a refusal, resolved {roots:?}")),
        Err(error) => Ok(error),
    }
}

#[test]
fn flattened_dependencies_use_the_primary_workspace_map() -> TestResult {
    let f = Fixture::new()?;
    let a = f.package("A")?;
    let b = a.join(".lake/packages/B");
    let c = a.join(".lake/packages/C");
    fs::create_dir_all(&b)?;
    fs::create_dir_all(&c)?;
    write_manifest(&a, &[git("B"), git("C")])?;
    write_manifest(&b, &[git("C")])?;
    write_manifest(&c, &[])?;
    assert!(!b.join(".lake/packages").exists());
    let roots = package_roots(&a)?;
    assert_eq!(roots.into_iter().collect::<BTreeSet<_>>(), BTreeSet::from([a, b, c]));
    Ok(())
}

#[test]
fn git_packages_in_a_subdirectory_resolve_to_that_subdirectory() -> TestResult {
    let f = Fixture::new()?;
    let a = f.package("A")?;
    let b = a.join(".lake/packages/B/lean");
    fs::create_dir_all(&b)?;
    write_manifest(&b, &[])?;
    write_manifest(
        &a,
        &[json!({"name":"B","type":"git","subDir":"lean","manifestFile":"lake-manifest.json"})],
    )?;
    assert_eq!(package_roots(&a)?, vec![a, b]);
    Ok(())
}

#[cfg(unix)]
#[test]
fn external_path_and_symlink_aliases_share_one_canonical_root() -> TestResult {
    let f = Fixture::new()?;
    let a = f.package("A")?;
    let b = f.package("B")?;
    let alias = f.0.join("alias");
    std::os::unix::fs::symlink(&b, &alias)?;
    write_manifest(&a, &[path("B", &b), path("Alias", &alias)])?;
    assert_eq!(package_roots(&a)?, vec![a, b]);
    Ok(())
}

#[test]
fn unsupported_or_missing_graph_metadata_never_degrades_to_primary_only() -> TestResult {
    let f = Fixture::new()?;
    let a = f.package("A")?;
    let b = f.package("B")?;
    write_manifest(&a, &[path("B", &b)])?;
    fs::remove_file(b.join("lake-manifest.json"))?;
    assert_eq!(refused(&a)?.kind(), io::ErrorKind::NotFound);
    write_manifest(&b, &[git("Absent")])?;
    assert!(refused(&a)?.to_string().contains("flattened"));
    write_manifest(&a, &[])?;
    fs::create_dir_all(a.join(".lake"))?;
    fs::write(a.join(".lake/package-overrides.json"), b"{\"B\":\"elsewhere\"}")?;
    assert!(refused(&a)?.to_string().contains("overrides"));
    fs::remove_file(a.join(".lake/package-overrides.json"))?;
    fs::write(a.join("lake-manifest.json"), b"{\"version\":\"2.0.0\",\"packages\":[]}")?;
    assert!(refused(&a)?.to_string().contains("schema"));
    Ok(())
}

#[test]
fn lake_projects_require_a_manifest_but_non_lake_inputs_are_their_own_closure() -> TestResult {
    let f = Fixture::new()?;
    let a = f.package("A")?;
    fs::remove_file(a.join("lake-manifest.json"))?;
    assert_eq!(package_roots(&a)?, vec![a.clone()]);
    fs::write(a.join("lakefile.toml"), b"name=\"a\"")?;
    assert!(refused(&a)?.to_string().contains("no dependency manifest"));
    Ok(())
}

#[cfg(unix)]
const FIFO_CHILD_ROOT: &str = "LEAN_TOOLCHAIN_FIFO_METADATA_TEST_ROOT";

#[cfg(unix)]
#[test]
#[ignore = "internal FIFO harness; the parent requires an explicit refusal marker"]
fn fifo_metadata_child() -> TestResult {
    let Some(root) = std::env::var_os(FIFO_CHILD_ROOT).map(PathBuf::from) else {
        return Ok(());
    };
    let error = refused(&root)?;
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("regular file"));
    fs::write(root.join("fifo-refused"), "nonregular metadata refused")?;
    Ok(())
}

#[cfg(unix)]
fn assert_fifo_metadata_refused(overrides: bool) -> TestResult {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let f = Fixture::new()?;
    let root = f.package("A")?;
    let metadata = if overrides {
        fs::create_dir(root.join(".lake"))?;
        root.join(".lake/package-overrides.json")
    } else {
        let manifest = root.join("lake-manifest.json");
        fs::remove_file(&manifest)?;
        manifest
    };
    if !Command::new("mkfifo")
        .arg("-m")
        .arg("600")
        .arg(&metadata)
        .status()?
        .success()
    {
        return Err("mkfifo failed".into());
    }
    // No writer opens this FIFO. A blocking open/read would hang indefinitely.
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "lake_packages::tests::fifo_metadata_child",
            "--ignored",
            "--nocapture",
        ])
        .env(FIFO_CHILD_ROOT, &root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or("test deadline overflows")?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Only this still-owned, unreaped Child is terminated.
                    let _termination = child.kill();
                    child.wait()?;
                    return Err("FIFO metadata discovery blocked; timeout is not a refusal".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    };
    assert!(status.success(), "FIFO child failed: {status}");
    assert_eq!(
        fs::read_to_string(root.join("fifo-refused"))?,
        "nonregular metadata refused"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn fifo_primary_manifest_is_refused_without_a_hang() -> TestResult {
    assert_fifo_metadata_refused(false)
}

#[cfg(unix)]
#[test]
fn fifo_package_overrides_are_refused_without_a_hang() -> TestResult {
    assert_fifo_metadata_refused(true)
}

#[cfg(unix)]
#[test]
fn regular_metadata_and_symlinks_to_regular_metadata_keep_their_semantics() -> TestResult {
    let f = Fixture::new()?;
    let root = f.package("A")?;
    fs::create_dir(root.join(".lake"))?;
    let overrides = root.join(".lake/package-overrides.json");
    fs::write(&overrides, "{}")?;
    assert_eq!(package_roots(&root)?, vec![root.clone()]);
    fs::rename(root.join("lake-manifest.json"), f.0.join("manifest.json"))?;
    std::os::unix::fs::symlink("../manifest.json", root.join("lake-manifest.json"))?;
    fs::rename(&overrides, f.0.join("overrides.json"))?;
    std::os::unix::fs::symlink("../../overrides.json", &overrides)?;
    assert_eq!(package_roots(&root)?, vec![root]);
    Ok(())
}

#[test]
fn regular_metadata_retains_the_exact_byte_bound() -> TestResult {
    let f = Fixture::new()?;
    let root = f.package("A")?;
    let manifest = root.join("lake-manifest.json");
    let mut bytes = fs::read(&manifest)?;
    bytes.resize(usize::try_from(MAX_MANIFEST_BYTES)?, b' ');
    fs::write(&manifest, &bytes)?;
    assert_eq!(package_roots(&root)?, vec![root.clone()]);
    bytes.push(b' ');
    fs::write(&manifest, &bytes)?;
    assert!(refused(&root)?.to_string().contains("byte bound"));
    Ok(())
}
