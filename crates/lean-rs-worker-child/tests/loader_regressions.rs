#![allow(unsafe_code, clippy::expect_used, clippy::panic)]

#[path = "support/shipped_template.rs"]
mod shipped_template;

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

fn template_target_debug() -> PathBuf {
    // One package-owned target lets Cargo reuse the template's outputs across test runs.
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("shipped-template")
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
    let debug = template_target_debug();
    let output = Command::new(cargo())
        .args(["build", "--jobs", "1", "--manifest-path"])
        .arg("Cargo.toml")
        .current_dir(shipped_template::root())
        .args(["--bins", "--examples", "--target-dir"])
        .arg(debug.parent().expect("debug output has a target directory"))
        .output()
        .expect("cargo build for shipped-lean-crate template starts");
    assert_success(output, "template cargo build");
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
fn shipped_template_launches_with_clean_loader_environment() {
    build_template();
    let debug = template_target_debug();

    let output = clean_loader_env_command(debug.join(exe_name("shipped-lean-crate")))
        .output()
        .expect("stripped-loader-env shipped-lean-crate binary starts");
    let stdout = assert_success(output, "stripped-loader-env shipped-lean-crate binary");
    assert!(
        stdout.contains("answer=42"),
        "same-process template should call Lean export, stdout={stdout:?}",
    );

    let worker_example = debug.join("examples").join(exe_name("worker"));
    let sibling = debug.join(exe_name("shipped-lean-crate-worker"));
    assert!(sibling.is_file(), "the default worker is installed");
    let output = clean_loader_env_command(&worker_example)
        .output()
        .expect("stripped-loader-env shipped-lean-crate worker example starts");
    let stdout = assert_success(output, "stripped-loader-env shipped-lean-crate worker example");
    assert!(
        stdout.contains("worker host session opened"),
        "worker template should open through the app-owned child, stdout={stdout:?}",
    );

    let missing = debug.join(exe_name("shipped-lean-crate-worker-missing-override"));
    assert!(!missing.try_exists().expect("inspect dedicated missing worker path"));
    let output = clean_loader_env_command(&worker_example)
        .env("SHIPPED_LEAN_CRATE_WORKER", &missing)
        .output()
        .expect("env-override shipped-lean-crate worker example starts");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "missing override must not fall back to the valid sibling"
    );
    assert!(
        stderr.contains(missing.to_str().expect("UTF-8 worker override path")),
        "worker startup diagnostic should name the rejected override, stderr={stderr:?}",
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("worker host session opened"),
        "worker must not open on the rejected override",
    );
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
    // SAFETY: this test owns the Lake-built consumer and interop images from the selected
    // toolchain. Their named initializers and dependency order match the fixture sources.
    unsafe {
        LeanCapability::open_with_dependencies(
            runtime,
            consumer_dylib_path(),
            "lean_rs_interop_consumer",
            "LeanRsInteropConsumer",
            [LeanLibraryDependency::path(interop_dylib_path())
                .export_symbols_for_dependents()
                .initializer("lean_rs_interop_shims", "LeanRsInterop")],
        )
    }
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
    // SAFETY: the fixture accepts the live registered callback and its matching trampoline;
    // both remain valid for the synchronous call and the fixture returns IO UInt8.
    let status =
        unsafe { callback_loop.call(handle, trampoline, 2) }.expect("callback loop runs after opener returned");

    assert_eq!(LeanCallbackStatus::from_abi(status), Some(LeanCallbackStatus::Ok));
    assert_eq!(
        seen.lock().expect("callback vector lock is not poisoned").as_slice(),
        &[(0, 2), (1, 2)],
    );
    assert_eq!(capability.bundle().dependency_count(), 1);
}
