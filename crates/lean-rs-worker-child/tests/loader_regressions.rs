#![allow(unsafe_code, clippy::expect_used, clippy::panic)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use lean_rs::module::LeanIo;
use lean_rs::{
    LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanCapability, LeanLibraryDependency, LeanProgressTick,
    LeanRuntime,
};

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn template_manifest() -> PathBuf {
    workspace_root()
        .join("templates")
        .join("shipped-lean-crate")
        .join("Cargo.toml")
}

fn template_target_debug() -> PathBuf {
    workspace_root()
        .join("templates")
        .join("shipped-lean-crate")
        .join("target")
        .join("debug")
}

fn exe_name(name: &str) -> String {
    let mut name = name.to_owned();
    if !std::env::consts::EXE_SUFFIX.is_empty() {
        name.push_str(std::env::consts::EXE_SUFFIX);
    }
    name
}

fn assert_success(output: Output, context: &str) -> String {
    assert!(
        output.status.success(),
        "{context} failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("test command stdout is valid UTF-8")
}

fn build_template() {
    // Memoize: every test in this binary needs the template built, but
    // running `cargo build` once per test multiplies rustc concurrency by
    // the nextest test-thread count and can exhaust developer-machine RAM
    // (each parallel cargo spawns its own rustc swarm linking
    // libleanshared). Build it exactly once per process, capping the
    // inner cargo's job count for the same reason.
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let output = Command::new(cargo())
            .args(["build", "--jobs", "1", "--manifest-path"])
            .arg(template_manifest())
            .args(["--bins", "--examples"])
            .output()
            .expect("cargo build for shipped-lean-crate template starts");
        assert_success(output, "template cargo build");
    });
}

fn clean_loader_env_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.env_remove("LD_LIBRARY_PATH");
    command.env_remove("LD_PRELOAD");
    command.env_remove("DYLD_LIBRARY_PATH");
    command.env_remove("DYLD_FALLBACK_LIBRARY_PATH");
    command.env_remove("DYLD_INSERT_LIBRARIES");
    command.env_remove("SHIPPED_LEAN_CRATE_WORKER");
    command
}

#[test]
fn stripped_loader_env_shipped_crate_binary_opens_manifest_capability() {
    build_template();

    let output = clean_loader_env_command(template_target_debug().join(exe_name("shipped-lean-crate")))
        .output()
        .expect("stripped-loader-env shipped-lean-crate binary starts");
    let stdout = assert_success(output, "stripped-loader-env shipped-lean-crate binary");

    assert!(
        stdout.contains("answer=42"),
        "same-process template should call Lean export, stdout={stdout:?}",
    );
}

#[test]
fn stripped_loader_env_shipped_crate_worker_example_uses_app_owned_child() {
    build_template();

    let output = clean_loader_env_command(template_target_debug().join("examples").join(exe_name("worker")))
        .output()
        .expect("stripped-loader-env shipped-lean-crate worker example starts");
    let stdout = assert_success(output, "stripped-loader-env shipped-lean-crate worker example");

    assert!(
        stdout.contains("worker capability opened"),
        "worker template should open through the app-owned child, stdout={stdout:?}",
    );
}

#[test]
fn shipped_crate_worker_example_honors_explicit_child_env_override() {
    build_template();

    let output = clean_loader_env_command(template_target_debug().join("examples").join(exe_name("worker")))
        .env(
            "SHIPPED_LEAN_CRATE_WORKER",
            template_target_debug().join(exe_name("shipped-lean-crate-worker")),
        )
        .output()
        .expect("env-override shipped-lean-crate worker example starts");
    let stdout = assert_success(output, "env-override shipped-lean-crate worker example");

    assert!(
        stdout.contains("worker capability opened"),
        "worker template should honor SHIPPED_LEAN_CRATE_WORKER override, stdout={stdout:?}",
    );
}

#[test]
fn shipped_crate_template_package_contains_lean_sources_and_worker_child() {
    let output = Command::new(cargo())
        .args(["package", "--manifest-path"])
        .arg(template_manifest())
        .args(["--allow-dirty", "--list"])
        .output()
        .expect("template cargo package --list starts");
    let stdout = assert_success(output, "template cargo package --list");

    for expected in [
        "build.rs",
        "examples/worker.rs",
        "lean/ShipLeanDemo.lean",
        "lean/lakefile.lean",
        "lean/lean-toolchain",
        "lean/lake-manifest.json",
        "src/bin/shipped_lean_crate_worker.rs",
        "src/main.rs",
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "template package list should include {expected}, got:\n{stdout}",
        );
    }
}

fn dylib_path(package_dir: &[&str], new_name: &str, old_name: &str) -> PathBuf {
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = package_dir
        .iter()
        .fold(workspace_root(), |path, part| path.join(part))
        .join(".lake")
        .join("build")
        .join("lib");
    let new_style = lib_dir.join(format!("{new_name}.{dylib_extension}"));
    let old_style = lib_dir.join(format!("{old_name}.{dylib_extension}"));
    if old_style.is_file() && !new_style.is_file() {
        old_style
    } else {
        new_style
    }
}

fn interop_dylib_path() -> PathBuf {
    dylib_path(
        &["crates", "lean-rs", "shims", "lean-rs-interop-shims"],
        "liblean__rs__interop__shims_LeanRsInterop",
        "libLeanRsInterop",
    )
}

fn consumer_dylib_path() -> PathBuf {
    dylib_path(
        &["fixtures", "interop-shims"],
        "liblean__rs__interop__consumer_LeanRsInteropConsumer",
        "libLeanRsInteropConsumer",
    )
}

fn open_consumer_capability() -> LeanCapability<'static> {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation succeeds");
    LeanCapability::open_with_dependencies(
        runtime,
        consumer_dylib_path(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        [LeanLibraryDependency::path(interop_dylib_path())
            .export_symbols_for_dependents()
            .initializer("lean_rs_interop_shims", "LeanRsInterop")],
    )
    .expect("consumer capability opens through public bundle loader")
}

#[test]
fn public_capability_bundle_keeps_transitive_dependency_after_opener_returns() {
    let capability = open_consumer_capability();
    let module = capability.module().expect("consumer module initializes");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let callback_loop = unsafe {
        module.exported_unchecked::<(usize, usize, u64), LeanIo<u8>>("lean_rs_interop_consumer_callback_loop")
    }
    .expect("callback loop export resolves");

    let seen = Arc::new(Mutex::new(Vec::new()));
    let callback_seen = Arc::clone(&seen);
    let callback = LeanCallbackHandle::<LeanProgressTick>::register(move |tick| {
        callback_seen
            .lock()
            .expect("callback vector lock is not poisoned")
            .push((tick.current, tick.total));
        LeanCallbackFlow::Continue
    })
    .expect("callback registration succeeds");

    let (handle, trampoline) = callback.abi_parts();
    let status = callback_loop
        .call(handle, trampoline, 2)
        .expect("callback loop runs after opener returned");

    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Ok));
    assert_eq!(
        seen.lock().expect("callback vector lock is not poisoned").as_slice(),
        &[(0, 2), (1, 2)],
    );
    assert_eq!(capability.bundle().dependency_count(), 1);
}
