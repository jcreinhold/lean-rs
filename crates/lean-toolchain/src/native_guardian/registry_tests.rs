use super::{OWNERS_DIRECTORY_ENV, bind_registry_directory, registry_directory, validate_registry_directory};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const CASE_ENV: &str = "LEAN_RS_TEST_REGISTRY_BINDING_CASE";
const ROOT_ENV: &str = "LEAN_RS_TEST_REGISTRY_BINDING_ROOT";

#[test]
fn registry_binding_fixes_one_read_only_namespace() -> io::Result<()> {
    let Some(root) = std::env::var_os(ROOT_ENV) else {
        // Each scenario gets a fresh process binding and uses only prospective paths.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()?
            .join(format!("absent-registry-fixture-{}", std::process::id()));
        for case in ["typed", "explicit-environment", "default"] {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args([
                    "--exact",
                    "native_guardian::registry_tests::registry_binding_fixes_one_read_only_namespace",
                    "--nocapture",
                ])
                .env(CASE_ENV, case)
                .env(ROOT_ENV, &root)
                .env("HOME", &root)
                .env("XDG_DATA_HOME", &root)
                .env_remove(OWNERS_DIRECTORY_ENV);
            if case == "explicit-environment" {
                command.env(OWNERS_DIRECTORY_ENV, root.join("selected"));
            }
            let output = command.output()?;
            assert!(
                output.status.success(),
                "{case}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(!root.try_exists()?);
        return Ok(());
    };
    let root = PathBuf::from(root);
    let selected = root.join("selected");
    let other = root.join("other");
    let case = std::env::var(CASE_ENV).map_err(io::Error::other)?;
    match case.as_str() {
        "typed" => {
            assert!(bind_registry_directory(Path::new("relative")).is_err());
            validate_registry_directory(&other)?;
            bind_registry_directory(&selected)?;
            bind_registry_directory(&selected)?;
            assert_eq!(registry_directory()?, selected);
            assert!(bind_registry_directory(&other).is_err());
        }
        "explicit-environment" => {
            let error = bind_registry_directory(&other)
                .err()
                .ok_or_else(|| io::Error::other("conflicting registry binding succeeded"))?;
            assert!(error.to_string().contains(OWNERS_DIRECTORY_ENV));
            bind_registry_directory(&selected)?;
            assert_eq!(registry_directory()?, selected);
        }
        "default" => {
            let expected = dirs::data_local_dir()
                .ok_or_else(|| io::Error::other("local data directory unavailable"))?
                .join("lean-rs/native-generations");
            assert_eq!(registry_directory()?, expected);
            assert!(bind_registry_directory(&selected).is_err());
            assert_eq!(registry_directory()?, expected);
        }
        _ => return Err(io::Error::other("unknown registry binding scenario")),
    }
    assert!(!root.try_exists()?);
    Ok(())
}
