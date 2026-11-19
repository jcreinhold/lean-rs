//! Private native fixtures compiled with the runtime’s selected Lean toolchain.
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(in crate::module) fn build_image(dir: &Path, filename: &str, modules: &[(&str, &str)]) -> PathBuf {
    let prefix = Path::new(lean_rs_sys::LEAN_HEADER_PATH)
        .ancestors()
        .nth(3)
        .expect("selected Lean prefix");
    let run = |tool, args: &[&str]| {
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
    };
    let image = format!("{filename}.{}", std::env::consts::DLL_EXTENSION);
    let mut c_sources = Vec::new();
    for (name, source) in modules {
        let input = format!("{name}.lean");
        let c = format!("{name}.c");
        fs::write(dir.join(&input), source).expect("write controlled Lean source");
        run("lean", &["-c", &c, &input]);
        c_sources.push(c);
    }
    let mut args = vec!["-shared", "-fPIC", "-DLEAN_EXPORTING", "-o", &image];
    args.extend(c_sources.iter().map(String::as_str));
    args.push("-lleanshared");
    run("leanc", &args);
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
