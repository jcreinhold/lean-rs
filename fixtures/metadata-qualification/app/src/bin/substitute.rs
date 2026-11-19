// Private negative controls. All mutations are inside this one fixture and run as the managed writer's target.
use lean_toolchain::shared_writer::SharedWriter;
use metadata_qualification::Record;
use std::{fs, path::Path, process::Command, time::Duration};
type E = Box<dyn std::error::Error>;
fn main() -> Result<(), E> {
    let d = Record::selected();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = args.first().ok_or("case")?.as_str();
    let root = match mode {
        "wrong-image" => d.interop_image.ancestors().nth(4).unwrap(),
        "wrong-initializer" => d.project.as_path(),
        "wrong-ordered-directories" => d.host_image.ancestors().nth(4).unwrap(),
        _ => return Err("unsupported control".into()),
    };
    let app = Path::new(env!("CARGO_MANIFEST_DIR"));
    let baseline = app.join("native-events.log");
    let output = if mode == "wrong-image" {
        &d.interop_image
    } else {
        &d.consumer_manifest
    };
    let backup = output.with_extension("qualification-backup");
    if let Some(action) = args.get(1) {
        if std::env::current_dir()? != root || !root.starts_with(app.parent().unwrap()) {
            return Err("outside fixture writer root".into());
        }
        if mode == "wrong-ordered-directories" {
            use std::{
                ffi::CString,
                os::unix::{ffi::OsStrExt, fs::MetadataExt},
            };
            if action != "install" && action != "restore" {
                return Err("unknown exchange stage".into());
            }
            let [left, right] = d.shim_paths.as_slice() else {
                return Err("expected two shim paths".into());
            };
            let expected = &d.ordered_directory_ids;
            let (before_left, before_right) = if action == "install" {
                (expected[2], expected[3])
            } else {
                (expected[3], expected[2])
            };
            let id = |p: &Path| -> Result<_, E> {
                let m = fs::symlink_metadata(p)?;
                Ok((m.dev(), m.ino()))
            };
            assert_eq!(id(left)?, before_left);
            assert_eq!(id(right)?, before_right);
            let from = CString::new(left.as_os_str().as_bytes())?;
            let to = CString::new(right.as_os_str().as_bytes())?;
            // SAFETY: both C strings live across this syscall. The managed writer holds
            // both exact private package domains. The atomic exchange preserves both trees.
            if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            assert_eq!(id(left)?, before_right);
            assert_eq!(id(right)?, before_left);
            return Ok(());
        }
        match action.as_str() {
            "install" => {
                assert!(!backup.exists());
                fs::rename(output, &backup)?;
                if mode == "wrong-image" {
                    fs::copy(&d.consumer_image, output)?;
                } else {
                    let mut altered: serde_json::Value = serde_json::from_str(&d.consumer_json)?;
                    altered["dependencies"][0]["initializer"]["module"] = "WrongInitializer".into();
                    fs::write(output, serde_json::to_vec_pretty(&altered)?)?;
                }
            }
            "restore" => {
                fs::rename(&backup, output)?;
            }
            _ => return Err("unsupported mutation".into()),
        }
        return Ok(());
    }
    let exec = std::env::current_exe()?;
    let mutate = |stage: &str| -> Result<(), E> {
        let mut writer = SharedWriter::open(&Record::helper(), root, Duration::from_secs(15))?;
        if mode == "wrong-ordered-directories" {
            assert_eq!(writer.write_roots(), d.shim_roots);
        }
        let mut cmd = Command::new(&exec);
        cmd.args([mode, stage]).current_dir(root);
        let result = writer.run(&cmd, Duration::from_secs(15))?;
        if !result.status.success() {
            return Err(format!("managed mutation failed: {result:?}").into());
        }
        Ok(())
    };
    let previous = fs::read_to_string(&baseline)?;
    mutate("install")?;
    let result = Command::new(exec.with_file_name("control"))
        .arg(mode)
        .output();
    mutate("restore")?;
    let result = result?;
    let after = fs::read_to_string(baseline)?;
    let events = after
        .strip_prefix(&previous)
        .ok_or("audit prefix changed")?;
    let wanted = if mode == "wrong-image" {
        "refused wrong file identity"
    } else if mode == "wrong-ordered-directories" {
        "refused wrong ordered import directory identity"
    } else {
        "refused consumer manifest or initializer differs"
    };
    println!(
        "status={} stdout={} stderr={} events={events}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.status.success()
            && events.lines().count() == 1
            && events.contains(wanted)
            && !events.contains("authorize-")
    );
    Ok(())
}
