use super::*;
use rustix::io::fcntl_getfd;
use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
use serde_json::{Value, json};
use std::io::{IoSlice, Write};
use std::os::fd::AsFd as _;
use std::sync::atomic::{AtomicUsize, Ordering};

static IPC_ROOT_NEXT: AtomicUsize = AtomicUsize::new(0);

pub(super) struct ExplicitIpcRoot(pub(super) PathBuf);
impl ExplicitIpcRoot {
    pub(super) fn new() -> io::Result<Self> {
        let base = if let Some(explicit) = std::env::var_os("LEAN_RS_TEST_IPC_BASE") {
            let base = PathBuf::from(explicit);
            validate_ipc_root(&base)?;
            if fs::symlink_metadata(&base)?.mode() & 0o7777 != 0o700 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "test IPC base must have mode 0700",
                ));
            }
            base
        } else {
            let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .ok_or_else(|| invalid("workspace"))?;
            let base = workspace.join("target");
            fs::create_dir_all(&base)?;
            base.canonicalize()?
        };
        let root = base.join(format!(
            "r{:x}{:02x}",
            std::process::id(),
            IPC_ROOT_NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // Fail before creating fixture state if the actual per-test path will
        // not fit the transport on this platform.
        SocketAddr::from_pathname(root.join("lean-read-000000000000000000000000/fd.sock"))?;
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        Ok(Self(root))
    }
}
impl Drop for ExplicitIpcRoot {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn explicit_ipc_root_owns_and_cleans_shared_transport_directory() -> io::Result<()> {
    let root = ExplicitIpcRoot::new()?;
    let (directory, _) = SocketDirectory::new(Some(&root.0))?;
    assert_eq!(directory.0.parent(), Some(root.0.as_path()));
    assert!(directory.0.is_dir());
    drop(directory);
    assert!(fs::read_dir(&root.0)?.next().is_none());
    Ok(())
}

#[test]
fn explicit_ipc_root_refuses_invalid_paths_before_creating_transport_state() -> io::Result<()> {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let relative = SocketDirectory::new(Some(Path::new("relative-ipc-root")))
        .err()
        .ok_or_else(|| invalid("accepted relative IPC root"))?;
    assert_eq!(relative.kind(), io::ErrorKind::InvalidInput);
    assert!(relative.to_string().contains("canonical, owned and mode 0700"));
    let root = ExplicitIpcRoot::new()?;
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o755))?;
    let mode = SocketDirectory::new(Some(&root.0))
        .err()
        .ok_or_else(|| invalid("accepted non-private IPC root"))?;
    assert_eq!(mode.kind(), io::ErrorKind::InvalidInput);
    assert!(mode.to_string().contains("canonical, owned and mode 0700"));
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700))?;
    let alias = root.0.join("alias");
    symlink(&root.0, &alias)?;
    let aliased = SocketDirectory::new(Some(&alias))
        .err()
        .ok_or_else(|| invalid("accepted aliased IPC root"))?;
    assert_eq!(aliased.kind(), io::ErrorKind::InvalidInput);
    assert!(aliased.to_string().contains("canonical, owned and mode 0700"));
    fs::remove_file(alias)?;
    assert!(fs::read_dir(&root.0)?.next().is_none());
    Ok(())
}

#[test]
fn explicit_ipc_root_refuses_overlong_socket_path_without_state() -> io::Result<()> {
    let root = ExplicitIpcRoot::new()?;
    let long = root.0.join("x".repeat(96));
    fs::DirBuilder::new().mode(0o700).create(&long)?;
    let long = long.canonicalize()?;
    assert_eq!(
        SocketDirectory::new(Some(&long))
            .err()
            .ok_or_else(|| invalid("accepted overlong IPC root"))?
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(fs::read_dir(&long)?.next().is_none());
    fs::remove_dir(long)?;
    Ok(())
}

struct Fixture(SocketDirectory);
impl Fixture {
    fn new() -> io::Result<Self> {
        let (directory, _) = SocketDirectory::new(None)?;
        fs::create_dir(directory.0.join(".lake"))?;
        Ok(Self(directory))
    }
    fn root(&self) -> &Path {
        &self.0.0
    }
    fn lock(&self) -> io::Result<PathBuf> {
        crate::shared_writer::project_lock_path(self.root())
    }
    fn file(&self) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.lock()?)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(self.root().join(".lake"));
        if let Ok(lock) = self.lock() {
            let _removed = fs::remove_file(lock);
        }
    }
}
fn receipt(fixture: &Fixture) -> io::Result<Value> {
    let metadata = fixture.file()?.metadata()?;
    Ok(
        json!({"protocol":1,"state":"read_ready","project_root":fixture.root(),"nonce":"nonce","epoch":"epoch",
        "lock_identity":{"device":metadata.dev(),"inode":metadata.ino()}}),
    )
}
fn sent(fixture: &Fixture, value: &Value, count: usize) -> io::Result<SharedReader> {
    let (sender, receiver) = UnixStream::pair()?;
    let files: Vec<File> = (0..count).map(|_| fixture.file()).collect::<io::Result<_>>()?;
    let descriptors: Vec<_> = files.iter().map(|file| file.as_fd()).collect();
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(8))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if count > 0 {
        assert!(ancillary.push(SendAncillaryMessage::ScmRights(&descriptors)));
    }
    let mut payload = serde_json::to_vec(value)?;
    payload.push(b'\n');
    sendmsg(&sender, &[IoSlice::new(&payload)], &mut ancillary, SendFlags::empty())?;
    drop(sender);
    receive(
        &receiver,
        fixture.root(),
        &fixture.lock()?,
        "nonce",
        Instant::now()
            .checked_add(Duration::from_secs(1))
            .ok_or_else(timed_out)?,
    )
}
#[test]
fn received_alias_has_cloexec_until_actual_body_inheritance() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let reader = sent(&fixture, &receipt(&fixture)?, 1)?;
    assert_eq!(reader.epoch(), "epoch");
    assert!(fcntl_getfd(&reader.file)?.contains(FdFlags::CLOEXEC));
    reader.inherit_by_body_children()?;
    assert!(!fcntl_getfd(&reader.file)?.contains(FdFlags::CLOEXEC));
    Ok(())
}
#[test]
fn receipt_identity_and_mode_mismatches_are_rejected() -> io::Result<()> {
    let fixture = Fixture::new()?;
    for (key, value) in [
        ("protocol", json!(2)),
        ("state", json!("ready")),
        ("project_root", json!("/")),
        ("nonce", json!("other")),
        ("epoch", json!("")),
        ("custody", json!("proxy")),
    ] {
        let mut frame = receipt(&fixture)?;
        frame
            .as_object_mut()
            .ok_or_else(|| invalid("fixture object"))?
            .insert(key.into(), value);
        assert!(sent(&fixture, &frame, 1).is_err(), "accepted {key}");
    }
    let mut frame = receipt(&fixture)?;
    *frame
        .get_mut("lock_identity")
        .and_then(|identity| identity.get_mut("inode"))
        .ok_or_else(|| invalid("fixture inode"))? = json!(0);
    assert!(sent(&fixture, &frame, 1).is_err());
    Ok(())
}
#[test]
fn absent_extra_and_truncated_descriptors_are_rejected() -> io::Result<()> {
    let fixture = Fixture::new()?;
    for count in [0, 2, 8] {
        assert!(sent(&fixture, &receipt(&fixture)?, count).is_err());
    }
    Ok(())
}
#[test]
fn fragmented_proxy_frame_and_invalid_boundaries() -> io::Result<()> {
    let (mut sender, receiver) = UnixStream::pair()?;
    receiver.set_nonblocking(true)?;
    let writer = thread::spawn(move || -> io::Result<()> {
        sender.write_all(b"{\"ok\":")?;
        thread::sleep(POLL);
        sender.write_all(b"true}\n")
    });
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .ok_or_else(timed_out)?;
    assert_eq!(read_line(receiver, deadline)?, b"{\"ok\":true}\n");
    writer.join().map_err(|_| invalid("test writer panicked"))??;
    for frame in [b"one\ntwo\n".as_slice(), b"incomplete".as_slice()] {
        assert!(read_line(frame, deadline).is_err());
    }
    let oversized = vec![b'x'; MAX_FRAME + 1];
    assert!(read_line(oversized.as_slice(), deadline).is_err());
    Ok(())
}
fn helper() -> io::Result<PathBuf> {
    std::env::var_os("LEAN_TOOLCHAIN_READER_TEST_HELPER")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("set explicit real reader helper path"))
}
fn blocked(file: &File) -> bool {
    file.try_lock().is_err()
}
fn wait_unlocked(file: &File) -> io::Result<()> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or_else(timed_out)?;
    loop {
        if file.try_lock().is_ok() {
            file.unlock()?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        thread::sleep(POLL);
    }
}
#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_writer_runs_command_in_selected_project() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let mut command = Command::new("/bin/pwd");
    command.current_dir(fixture.root());
    let output = crate::shared_writer::run_shared_writer(&helper()?, fixture.root(), &command, Duration::from_secs(5))?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        Path::new(String::from_utf8_lossy(&output.stdout).trim()).canonicalize()?,
        fixture.root().canonicalize()?
    );
    wait_unlocked(&fixture.file()?)
}

#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_direct_readers_hold_independent_aliases() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let helper = helper()?;
    let first = SharedReader::acquire(&helper, fixture.root(), Duration::from_secs(2))?;
    let second = SharedReader::acquire(&helper, fixture.root(), Duration::from_secs(2))?;
    assert_eq!(first.epoch(), second.epoch());
    let writer = fixture.file()?;
    assert!(blocked(&writer));
    drop(first);
    assert!(blocked(&writer));
    drop(second);
    wait_unlocked(&writer)
}
#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_body_child_keeps_actual_read_alias_after_caller_drop() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let reader = SharedReader::acquire(&helper()?, fixture.root(), Duration::from_secs(2))?;
    reader.inherit_by_body_children()?;
    let mut child = Command::new("/bin/sleep").arg("0.15").spawn()?;
    let writer = fixture.file()?;
    drop(reader);
    assert!(blocked(&writer));
    assert!(child.wait()?.success());
    wait_unlocked(&writer)
}
#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_fifo_proxy_does_not_leak_custody_to_unrelated_spawn() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let mut proxy = SharedReadProxy::acquire(&helper()?, fixture.root(), Duration::from_secs(2))?;
    assert!(!proxy.epoch().is_empty());
    proxy.validate()?;
    let mut unrelated = Command::new("/bin/sleep").arg("0.5").spawn()?;
    let writer = fixture.file()?;
    assert!(blocked(&writer));
    drop(proxy);
    wait_unlocked(&writer)?;
    assert!(unrelated.try_wait()?.is_none());
    assert!(unrelated.wait()?.success());
    Ok(())
}
#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_proxy_failure_invalidates_cache_acceptance() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let mut proxy = SharedReadProxy::acquire(&helper()?, fixture.root(), Duration::from_secs(2))?;
    let child = proxy.helper.as_mut().ok_or_else(|| invalid("helper missing"))?;
    child.kill()?;
    child.wait()?;
    assert!(proxy.validate().is_err());
    Ok(())
}
#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_writer_conflict_times_out_without_releasing_writer() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let writer = fixture.file()?;
    writer.lock()?;
    for proxy in [false, true] {
        let start = Instant::now();
        let failed = if proxy {
            SharedReadProxy::acquire(&helper()?, fixture.root(), Duration::from_millis(40)).map(|_| ())
        } else {
            SharedReader::acquire(&helper()?, fixture.root(), Duration::from_millis(40)).map(|_| ())
        };
        assert!(failed.is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(blocked(&fixture.file()?));
    }
    writer.unlock()?;
    Ok(())
}

#[test]
fn delayed_proxy_helper_is_reaped_after_bounded_drop() -> io::Result<()> {
    let (directory, _) = SocketDirectory::new(None)?;
    let reaper = reserve_reaper()?;
    let child = Command::new("/bin/sleep").arg("0.3").spawn()?;
    let pid = rustix::process::Pid::from_child(&child);
    let proxy = SharedReadProxy {
        control: None,
        helper: Some(child),
        _directory: directory,
        epoch: "fixture".to_owned(),
        reaper,
        diagnostics: Diagnostics::empty(),
    };
    let start = Instant::now();
    drop(proxy);
    assert!(start.elapsed() < Duration::from_millis(250));
    // This process lasts longer than the bounded local cleanup window. The
    // pre-created reaper must consume its status without a later API call.
    thread::sleep(Duration::from_millis(500));
    assert!(matches!(
        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
    Ok(())
}

#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_readers_exclude_current_shared_writer_client_before_helper_launch() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let helper = helper()?;
    let reader = SharedReader::acquire(&helper, fixture.root(), Duration::from_secs(2))?;
    let error = crate::shared_writer::SharedWriter::open(&helper, fixture.root(), Duration::from_millis(40))
        .err()
        .ok_or_else(|| invalid("writer admitted across worker reader"))?;
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        crate::shared_writer::WriterFailure::from_io(&error).map(|failure| failure.disposition),
        Some(crate::shared_writer::WriterDisposition::NotStarted)
    );
    drop(reader);
    let mut proxy = SharedReadProxy::acquire(&helper, fixture.root(), Duration::from_secs(2))?;
    let error = crate::shared_writer::SharedWriter::open(&helper, fixture.root(), Duration::from_millis(40))
        .err()
        .ok_or_else(|| invalid("writer admitted across parent proxy"))?;
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    proxy.validate()?;
    Ok(())
}

#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_reader_domain_survives_lake_directory_replacement() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let helper = helper()?;
    let original = SharedReader::acquire(&helper, fixture.root(), Duration::from_secs(2))?;
    fs::remove_dir(fixture.root().join(".lake"))?;
    fs::create_dir(fixture.root().join(".lake"))?;
    let mut proxy = SharedReadProxy::acquire(&helper, fixture.root(), Duration::from_secs(2))?;
    assert_eq!(original.epoch(), proxy.epoch());
    let writer = fixture.file()?;
    assert!(blocked(&writer));
    drop(original);
    assert!(blocked(&writer));
    proxy.validate()?;
    drop(proxy);
    wait_unlocked(&writer)
}

#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_thin_writer_controller_excludes_both_read_clients() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new()?;
    let fake_helper = fixture.root().join("pending-writer-helper");
    fs::write(&fake_helper, "#!/bin/sh\nexec /bin/sleep 0.5\n")?;
    fs::set_permissions(&fake_helper, fs::Permissions::from_mode(0o700))?;
    let root = fixture.root().to_path_buf();
    let writer = thread::spawn(move || {
        crate::shared_writer::SharedWriter::open(&fake_helper, &root, Duration::from_millis(400)).map(drop)
    });
    let lock = fixture.file()?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .ok_or_else(timed_out)?;
    loop {
        if blocked(&lock) {
            break;
        }
        lock.unlock()?;
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        thread::sleep(POLL);
    }
    assert!(SharedReader::acquire(&helper()?, fixture.root(), Duration::from_millis(40)).is_err());
    assert!(SharedReadProxy::acquire(&helper()?, fixture.root(), Duration::from_millis(40)).is_err());
    let refused = writer.join().map_err(|_| invalid("writer fixture panicked"))?;
    assert!(refused.is_err());
    fs::remove_file(fixture.root().join("pending-writer-helper"))?;
    wait_unlocked(&lock)
}

#[test]
fn cold_reader_roots_do_not_require_or_create_a_lake_directory() -> io::Result<()> {
    let (directory, _) = SocketDirectory::new(None)?;
    validate_lake_if_present(&directory.0)?;
    assert!(!directory.0.join(".lake").exists());
    fs::create_dir(directory.0.join(".lake"))?;
    validate_lake_if_present(&directory.0)?;
    fs::remove_dir(directory.0.join(".lake"))?;
    Ok(())
}

#[test]
fn existing_aliased_or_non_directory_lake_roots_are_rejected() -> io::Result<()> {
    let (directory, _) = SocketDirectory::new(None)?;
    let (other, _) = SocketDirectory::new(None)?;
    let lake = directory.0.join(".lake");
    fs::write(&lake, b"not a directory")?;
    assert_eq!(
        validate_lake_if_present(&directory.0)
            .err()
            .ok_or_else(|| invalid("accepted file"))?
            .kind(),
        io::ErrorKind::InvalidData
    );
    fs::remove_file(&lake)?;
    std::os::unix::fs::symlink(&other.0, &lake)?;
    assert_eq!(
        validate_lake_if_present(&directory.0)
            .err()
            .ok_or_else(|| invalid("accepted alias"))?
            .kind(),
        io::ErrorKind::InvalidData
    );
    fs::remove_file(&lake)?;
    std::os::unix::fs::symlink(other.0.join("missing"), &lake)?;
    assert_eq!(
        validate_lake_if_present(&directory.0)
            .err()
            .ok_or_else(|| invalid("accepted dangling alias"))?
            .kind(),
        io::ErrorKind::InvalidData
    );
    fs::remove_file(lake)?;
    Ok(())
}

#[test]
#[ignore = "requires explicit LEAN_TOOLCHAIN_READER_TEST_HELPER; run with one test thread"]
fn real_descriptor_and_proxy_readers_lock_cold_roots_without_build_writes() -> io::Result<()> {
    let fixture = Fixture::new()?;
    fs::remove_dir(fixture.root().join(".lake"))?;
    let direct = SharedReader::acquire(&helper()?, fixture.root(), Duration::from_secs(2))?;
    let mut proxy = SharedReadProxy::acquire(&helper()?, fixture.root(), Duration::from_secs(2))?;
    proxy.validate()?;
    let writer = fixture.file()?;
    assert!(blocked(&writer));
    drop(direct);
    assert!(blocked(&writer));
    drop(proxy);
    wait_unlocked(&writer)?;
    assert!(!fixture.root().join(".lake").exists());
    Ok(())
}
