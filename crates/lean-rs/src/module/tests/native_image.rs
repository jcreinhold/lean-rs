//! Private native fixtures compiled with the runtime’s selected Lean toolchain.
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(in crate::module) fn build_image(dir: &Path, filename: &str, modules: &[(&str, &str)]) -> PathBuf {
    let mut c_sources = Vec::new();
    for (name, source) in modules {
        let input = format!("{name}.lean");
        let c = format!("{name}.c");
        fs::write(dir.join(&input), source).expect("write controlled Lean source");
        run(dir, "lean", &["-c", &c, &input]);
        c_sources.push(c);
    }
    link_image(dir, filename, &c_sources)
}

pub(in crate::module) fn link_image(dir: &Path, filename: &str, c_sources: &[impl AsRef<str>]) -> PathBuf {
    let image = format!("{filename}.{}", std::env::consts::DLL_EXTENSION);
    let mut args = vec!["-shared", "-fPIC", "-DLEAN_EXPORTING", "-o", &image];
    args.extend(c_sources.iter().map(AsRef::as_ref));
    args.push("-lleanshared");
    run(dir, "leanc", &args);
    dir.join(image)
}

pub(in crate::module) fn private_dir() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("lean-rs-image-{}-{stamp}", std::process::id()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .expect("create exclusive fixture directory");
    dir
}

fn run(dir: &Path, tool: &str, args: &[&str]) {
    let prefix = Path::new(lean_rs_sys::LEAN_HEADER_PATH)
        .ancestors()
        .nth(3)
        .expect("selected Lean prefix");

    let output = Command::new(prefix.join("bin").join(tool))
        .current_dir(dir)
        .env("LEAN_SYSROOT", prefix)
        .env_remove("LEAN_PATH")
        .args(args)
        .output()
        .expect("run selected Lean toolchain");
    assert!(
        output.status.success(),
        "{tool} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
