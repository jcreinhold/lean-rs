use std::path::PathBuf;
use std::process::Command;

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
}

pub fn results_dir() -> PathBuf {
    workspace_root().join("profiling_results")
}

pub fn target_profile_dir() -> PathBuf {
    workspace_root().join("target").join("profiling")
}

pub fn profiling_example(name: &str) -> PathBuf {
    target_profile_dir()
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

pub fn profiling_bin(name: &str) -> PathBuf {
    target_profile_dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

pub fn git_output(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(workspace_root())
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            } else {
                None
            }
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

pub fn timestamp_utc() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            } else {
                None
            }
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

pub fn platform() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}
