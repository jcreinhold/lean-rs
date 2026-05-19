//! Reusable build-script helpers for downstream embedders.
//!
//! Inside this workspace `lean-rs-sys`'s `build.rs` is the single source of
//! `cargo:rustc-link-*` directives — `lean-toolchain` does not call into the
//! helper from its own build script. The helper exists for **downstream
//! embedders** whose own `build.rs` would otherwise duplicate the link-policy
//! probe, the directive set, and the runtime rpath logic.
//!
//! Usage in a downstream `build.rs` for a pure Rust-to-Lean export consumer:
//!
//! ```ignore
//! use std::path::Path;
//!
//! fn main() {
//!     lean_toolchain::emit_lean_link_directives();
//!     let dylib = lean_toolchain::build_lake_target(Path::new("lean"), "MyCapability")?;
//!     println!("cargo:rustc-env=MY_CAPABILITY_DYLIB={}", dylib.display());
//!     Ok::<(), Box<dyn std::error::Error>>(())
//! }
//! ```
//!
//! That one call covers link-time (the `cargo:rustc-link-search` /
//! `link-lib` directives) and load-time (the rpath into the Lean toolchain's
//! `lib/lean` directory) so a consumer binary runs without
//! `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH` set.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::diagnostics::LinkDiagnostics;
use crate::discover::{DiscoverOptions, ToolchainInfo, discover_toolchain};

/// Set once on the first call to make repeat calls (e.g. multiple
/// `build.rs` invocations within one process) cheap and idempotent.
static EMITTED: OnceLock<()> = OnceLock::new();

/// Emit Lean link-search, link-lib, and runtime rpath directives plus
/// matching rerun triggers from a downstream `build.rs`.
///
/// On the first call this:
///
/// 1. Runs [`discover_toolchain`] with [`DiscoverOptions::default()`].
/// 2. On success, prints `cargo:rustc-link-search=native=<prefix>/lib/lean`
///    and `<prefix>/lib`, plus `cargo:rustc-link-lib=dylib=leanshared`.
/// 3. On macOS or Linux build targets, also prints
///    `cargo:rustc-link-arg=-Wl,-rpath,<prefix>/lib/lean` so the resulting
///    binary loads `libleanshared` without `DYLD_FALLBACK_LIBRARY_PATH` /
///    `LD_LIBRARY_PATH`. Other targets get no rpath directive.
/// 4. On discovery failure, prints one `cargo:warning=` line with the
///    formatted diagnostic and returns; the caller's build then fails at
///    link time with a more specific error from rustc.
/// 5. Emits `cargo:rerun-if-changed=<header>` and the env-var triggers
///    discovery consults.
///
/// Subsequent calls within the same process are no-ops.
pub fn emit_lean_link_directives() {
    if EMITTED.set(()).is_err() {
        return;
    }

    match discover_toolchain(&DiscoverOptions::default()) {
        Ok(info) => emit_for(&info),
        Err(diagnostic) => {
            println!("cargo:warning={diagnostic}");
            emit_rerun_triggers(None);
        }
    }
}

/// Build a Lake `lean_lib` shared-library target and return the produced dylib path.
///
/// `project_root` must be the directory containing `lakefile.lean`. `target_name` is the
/// Lake target name to build; the helper invokes `lake build <target_name>:shared` on a
/// cache miss and returns the supported-window dylib path under
/// `<project_root>/.lake/build/lib/`.
///
/// The cache key is:
///
/// - SHA-256 of `lake-manifest.json`;
/// - the maximum modification timestamp of `lakefile.lean`, `lakefile.toml`, `lean-toolchain`,
///   and every `*.lean` file below `project_root` excluding `.lake/`;
/// - the counted source-set size;
/// - the target name and Lake package name.
///
/// A cache hit skips the Lake command only when the cache key matches and the dylib exists.
/// The helper always emits `cargo:rerun-if-changed=...` directives for the Lake files and
/// source files it scans. It captures Lake stdout/stderr and never forwards Lake output to
/// stdout, so stdout remains valid Cargo build-script directives only.
///
/// # Errors
///
/// Returns [`LinkDiagnostics::LakeTargetMissing`] if `target_name` is not declared as a
/// `lean_lib` in `lakefile.lean`, [`LinkDiagnostics::LakeBuildFailed`] if Lake exits
/// unsuccessfully, and [`LinkDiagnostics::LakeOutputUnresolved`] for unreadable manifests,
/// source-set traversal failures, cache write failures, or missing built dylibs.
pub fn build_lake_target(project_root: &Path, target_name: &str) -> Result<PathBuf, LinkDiagnostics> {
    let mut runner = RealLakeRunner;
    build_lake_target_with_runner(project_root, target_name, &mut runner)
}

fn emit_for(info: &ToolchainInfo) {
    let lib_lean = info.lib_dir.join("lean");
    println!("cargo:rustc-link-search=native={}", lib_lean.display());
    println!("cargo:rustc-link-search=native={}", info.lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=leanshared");

    // Runtime rpath so the consumer binary finds `libleanshared` without
    // `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH`. Gated on the build
    // target (not the host) via `CARGO_CFG_TARGET_OS`; `-Wl,-rpath` is a
    // GNU-ld / lld / Apple-ld flag and is not meaningful on Windows.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "macos" | "linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_lean.display());
    }

    emit_rerun_triggers(Some(info));
}

fn emit_rerun_triggers(info: Option<&ToolchainInfo>) {
    if let Some(info) = info {
        println!("cargo:rerun-if-changed={}", info.header_path.display());
    }
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=ELAN_HOME");
    println!("cargo:rerun-if-env-changed=PATH");
}

trait LakeRunner {
    fn build_shared(&mut self, project_root: &Path, target_name: &str) -> Result<LakeRun, std::io::Error>;
}

struct RealLakeRunner;

impl LakeRunner for RealLakeRunner {
    fn build_shared(&mut self, project_root: &Path, target_name: &str) -> Result<LakeRun, std::io::Error> {
        let output = Command::new("lake")
            .arg("build")
            .arg(format!("{target_name}:shared"))
            .current_dir(project_root)
            .output()?;
        Ok(LakeRun {
            success: output.status.success(),
            status: output.status.to_string(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

struct LakeRun {
    success: bool,
    status: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn build_lake_target_with_runner(
    project_root: &Path,
    target_name: &str,
    runner: &mut impl LakeRunner,
) -> Result<PathBuf, LinkDiagnostics> {
    let project_root = project_root.to_path_buf();
    let lakefile = project_root.join("lakefile.lean");
    println!("cargo:rerun-if-changed={}", lakefile.display());
    let lakefile_toml = project_root.join("lakefile.toml");
    if lakefile_toml.is_file() {
        println!("cargo:rerun-if-changed={}", lakefile_toml.display());
    }
    let toolchain_file = project_root.join("lean-toolchain");
    if toolchain_file.is_file() {
        println!("cargo:rerun-if-changed={}", toolchain_file.display());
    }

    if !target_declared_in_lakefile(&lakefile, target_name)? {
        return Err(LinkDiagnostics::LakeTargetMissing {
            project_root,
            target_name: target_name.to_owned(),
        });
    }

    let manifest_path = project_root.join("lake-manifest.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let manifest_bytes = fs::read(&manifest_path).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.clone(),
        target_name: target_name.to_owned(),
        reason: format!("could not read {} ({err})", manifest_path.display()),
    })?;
    let manifest_digest = sha256_hex(&manifest_bytes);
    let package_name = package_name_from_manifest(&project_root, target_name, &manifest_path, &manifest_bytes)?;
    let source_set = scan_source_set(&project_root, target_name)?;
    for path in &source_set.paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let dylib = resolve_dylib_path(&project_root, &package_name, target_name);
    let cache_key = cache_key(target_name, &package_name, &manifest_digest, &source_set);
    let cache_path = cache_path(&project_root, target_name);
    if dylib.is_file() && fs::read_to_string(&cache_path).is_ok_and(|cached| cached == cache_key) {
        return Ok(dylib);
    }

    let run = runner
        .build_shared(&project_root, target_name)
        .map_err(|err| LinkDiagnostics::LakeBuildFailed {
            project_root: project_root.clone(),
            target_name: target_name.to_owned(),
            status: "failed to spawn".to_owned(),
            detail: err.to_string(),
        })?;
    if !run.success {
        return Err(LinkDiagnostics::LakeBuildFailed {
            project_root,
            target_name: target_name.to_owned(),
            status: run.status,
            detail: command_detail(&run.stdout, &run.stderr),
        });
    }

    let dylib = resolve_dylib_path(&project_root, &package_name, target_name);
    if !dylib.is_file() {
        return Err(LinkDiagnostics::LakeOutputUnresolved {
            project_root,
            target_name: target_name.to_owned(),
            reason: format!("expected shared library at {}", dylib.display()),
        });
    }

    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.clone(),
            target_name: target_name.to_owned(),
            reason: format!("could not create cache directory {} ({err})", parent.display()),
        })?;
    }
    fs::write(&cache_path, cache_key).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root,
        target_name: target_name.to_owned(),
        reason: format!("could not write cache file {} ({err})", cache_path.display()),
    })?;

    Ok(dylib)
}

fn target_declared_in_lakefile(lakefile: &Path, target_name: &str) -> Result<bool, LinkDiagnostics> {
    let contents = fs::read_to_string(lakefile).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: lakefile.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not read {} ({err})", lakefile.display()),
    })?;
    let quoted = format!("lean_lib «{target_name}»");
    let bare = format!("lean_lib {target_name}");
    let string = format!("lean_lib \"{target_name}\"");
    Ok(contents.contains(&quoted) || contents.contains(&bare) || contents.contains(&string))
}

fn package_name_from_manifest(
    project_root: &Path,
    target_name: &str,
    manifest_path: &Path,
    manifest_bytes: &[u8],
) -> Result<String, LinkDiagnostics> {
    let manifest: serde_json::Value =
        serde_json::from_slice(manifest_bytes).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("{} is not valid JSON ({err})", manifest_path.display()),
        })?;
    manifest
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("{} has no string `name` field", manifest_path.display()),
        })
}

struct SourceSet {
    paths: Vec<PathBuf>,
    max_mtime_ns: u128,
}

fn scan_source_set(project_root: &Path, target_name: &str) -> Result<SourceSet, LinkDiagnostics> {
    let mut paths = Vec::new();
    collect_lean_sources(project_root, project_root, target_name, &mut paths)?;
    for file_name in ["lakefile.lean", "lakefile.toml", "lean-toolchain"] {
        let path = project_root.join(file_name);
        if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();

    let mut max_mtime_ns = 0;
    for path in &paths {
        let metadata = fs::metadata(path).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not stat {} ({err})", path.display()),
        })?;
        let modified = metadata
            .modified()
            .map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
                project_root: project_root.to_path_buf(),
                target_name: target_name.to_owned(),
                reason: format!("could not read mtime for {} ({err})", path.display()),
            })?;
        let mtime_ns = modified
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        max_mtime_ns = max_mtime_ns.max(mtime_ns);
    }

    Ok(SourceSet { paths, max_mtime_ns })
}

fn collect_lean_sources(
    project_root: &Path,
    dir: &Path,
    target_name: &str,
    paths: &mut Vec<PathBuf>,
) -> Result<(), LinkDiagnostics> {
    for entry in fs::read_dir(dir).map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
        project_root: project_root.to_path_buf(),
        target_name: target_name.to_owned(),
        reason: format!("could not read directory {} ({err})", dir.display()),
    })? {
        let entry = entry.map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not read directory entry under {} ({err})", dir.display()),
        })?;
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == ".lake") {
            continue;
        }
        let metadata = entry.metadata().map_err(|err| LinkDiagnostics::LakeOutputUnresolved {
            project_root: project_root.to_path_buf(),
            target_name: target_name.to_owned(),
            reason: format!("could not stat {} ({err})", path.display()),
        })?;
        if metadata.is_dir() {
            collect_lean_sources(project_root, &path, target_name, paths)?;
        } else if path.extension().is_some_and(|ext| ext == "lean") {
            paths.push(path);
        }
    }
    Ok(())
}

fn resolve_dylib_path(project_root: &Path, package_name: &str, target_name: &str) -> PathBuf {
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = project_root.join(".lake").join("build").join("lib");
    let escaped_package = package_name.replace('_', "__");
    let new_style = lib_dir.join(format!("lib{escaped_package}_{target_name}.{dylib_extension}"));
    let old_style = lib_dir.join(format!("lib{target_name}.{dylib_extension}"));
    if new_style.is_file() {
        new_style
    } else if old_style.is_file() {
        old_style
    } else {
        new_style
    }
}

fn cache_path(project_root: &Path, target_name: &str) -> PathBuf {
    project_root
        .join(".lake")
        .join("lean-rs-build-cache")
        .join(format!("{}.cache", sanitize_target_name(target_name)))
}

fn sanitize_target_name(target_name: &str) -> String {
    target_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn cache_key(target_name: &str, package_name: &str, manifest_digest: &str, source_set: &SourceSet) -> String {
    format!(
        "target={target_name}\npackage={package_name}\nmanifest={manifest_digest}\nsource_count={}\nsource_max_mtime_ns={}\n",
        source_set.paths.len(),
        source_set.max_mtime_ns
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn command_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut raw = String::new();
    if !stderr.is_empty() {
        raw.push_str(&String::from_utf8_lossy(stderr));
    }
    if !stdout.is_empty() {
        if !raw.is_empty() {
            raw.push_str(" | ");
        }
        raw.push_str(&String::from_utf8_lossy(stdout));
    }
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "no output".to_owned()
    } else if collapsed.len() > 1024 {
        let mut bounded = collapsed.chars().take(1024).collect::<String>();
        bounded.push_str("...");
        bounded
    } else {
        collapsed
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]
mod tests {
    use super::{LakeRun, LakeRunner, build_lake_target_with_runner, command_detail};
    use crate::LinkDiagnostics;
    use std::cell::Cell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    #[derive(Clone)]
    struct FakeLake {
        calls: Rc<Cell<usize>>,
        mode: FakeMode,
    }

    #[derive(Clone)]
    enum FakeMode {
        SuccessModern,
        SuccessLegacy,
        Failure,
    }

    impl FakeLake {
        fn new(mode: FakeMode) -> Self {
            Self {
                calls: Rc::new(Cell::new(0)),
                mode,
            }
        }

        fn calls(&self) -> usize {
            self.calls.get()
        }
    }

    impl LakeRunner for FakeLake {
        fn build_shared(&mut self, project_root: &Path, target_name: &str) -> Result<LakeRun, std::io::Error> {
            self.calls.set(self.calls.get().saturating_add(1));
            match self.mode {
                FakeMode::SuccessModern => {
                    let dylib = project_root
                        .join(".lake")
                        .join("build")
                        .join("lib")
                        .join(format!("libmy__pkg_{target_name}.{}", dylib_ext()));
                    write_file(&dylib, "dylib");
                    Ok(success_run())
                }
                FakeMode::SuccessLegacy => {
                    let dylib = project_root
                        .join(".lake")
                        .join("build")
                        .join("lib")
                        .join(format!("lib{target_name}.{}", dylib_ext()));
                    write_file(&dylib, "dylib");
                    Ok(success_run())
                }
                FakeMode::Failure => Ok(LakeRun {
                    success: false,
                    status: "exit status: 1".to_owned(),
                    stdout: b"stdout detail\n".to_vec(),
                    stderr: b"stderr detail\n".to_vec(),
                }),
            }
        }
    }

    fn success_run() -> LakeRun {
        LakeRun {
            success: true,
            status: "exit status: 0".to_owned(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn dylib_ext() -> &'static str {
        if cfg!(target_os = "macos") { "dylib" } else { "so" }
    }

    fn make_project(name: &str, target: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lean-toolchain-lake-{}-{}", std::process::id(), name));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).expect("create temp project");
        write_file(
            &root.join("lakefile.lean"),
            &format!(
                "import Lake\nopen Lake DSL\npackage «my_pkg»\n@[default_target]\nlean_lib «{target}» where\n  defaultFacets := #[LeanLib.sharedFacet]\n"
            ),
        );
        write_file(
            &root.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packagesDir":".lake/packages","packages":[],"name":"my_pkg","lakeDir":".lake"}"#,
        );
        write_file(&root.join("lean-toolchain"), "leanprover/lean4:v4.29.1\n");
        write_file(&root.join(format!("{target}.lean")), "def hello : Nat := 1\n");
        root
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn cache_hit_skips_lake_invocation() {
        let root = make_project("cache-hit", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let first = build_lake_target_with_runner(&root, "MyCapability", &mut runner).expect("first build");
        let second = build_lake_target_with_runner(&root, "MyCapability", &mut runner).expect("cached build");

        assert_eq!(first, second);
        assert_eq!(runner.calls(), 1, "second call should use cache");
    }

    #[test]
    fn legacy_output_path_is_supported() {
        let root = make_project("legacy", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessLegacy);
        let path = build_lake_target_with_runner(&root, "MyCapability", &mut runner).expect("legacy build");

        assert!(path.ends_with(format!("libMyCapability.{}", dylib_ext())));
    }

    #[test]
    fn missing_target_is_typed() {
        let root = make_project("missing-target", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::SuccessModern);
        let err = build_lake_target_with_runner(&root, "OtherTarget", &mut runner).expect_err("missing target");

        match err {
            LinkDiagnostics::LakeTargetMissing { target_name, .. } => assert_eq!(target_name, "OtherTarget"),
            other => panic!("expected LakeTargetMissing, got {other:?}"),
        }
        assert_eq!(runner.calls(), 0);
    }

    #[test]
    fn build_failure_is_typed_and_one_line() {
        let root = make_project("failure", "MyCapability");
        let mut runner = FakeLake::new(FakeMode::Failure);
        let err = build_lake_target_with_runner(&root, "MyCapability", &mut runner).expect_err("failure");
        let rendered = format!("{err}");

        match err {
            LinkDiagnostics::LakeBuildFailed { detail, .. } => {
                assert!(detail.contains("stderr detail"));
                assert!(detail.contains("stdout detail"));
                assert!(!detail.contains('\n'));
            }
            other => panic!("expected LakeBuildFailed, got {other:?}"),
        }
        assert!(!rendered.contains('\n'));
    }

    #[test]
    fn command_detail_is_bounded() {
        let detail = command_detail(&vec![b'x'; 4096], b"");
        assert!(detail.len() <= 1027);
        assert!(detail.ends_with("..."));
    }
}
