// Linux dispatch stays unavailable until its release, helper and worker inputs
// have independently verified pins and the managed guardian route is qualified.
#![cfg(all(feature = "admitted", target_os = "macos"))]
use std::process::Command;

fn check(mode: &str, expected: &str) {
    // Cargo may supply a fallback loader search path. Launch the selected binary
    // with its own built-in rpath and the same closed environment as the application.
    let result = Command::new(env!("CARGO_BIN_EXE_control"))
        .arg(mode)
        .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
        .env_remove("DYLD_LIBRARY_PATH")
        .output()
        .expect("start the private metadata application");
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let message = String::from_utf8(result.stdout).expect("test output");
    assert!(message.contains(expected), "{message}");
}

#[test]
fn admitted_export_returns_complete_metadata() {
    check("positive", "complete native metadata");
}

#[test]
fn changed_expectation_returns_capability_metadata_mismatch() {
    check("mismatch", "CapabilityMetadataMismatch after exact export");
}
