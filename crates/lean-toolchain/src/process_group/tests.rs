#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "isolated process fixtures assert exact lifecycle and syscall outcomes"
)]

use super::{OwnedCommandGroup, macos};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const LIMIT: Duration = Duration::from_secs(5);
const SCRIPT: &str = r"
import os, pathlib, signal, sys, time
root = pathlib.Path(sys.argv[1])
mode, count = sys.argv[2], int(sys.argv[3])
signal.signal(signal.SIGTERM, signal.SIG_IGN)
children = []
for index in range(count):
    child = os.fork()
    if child == 0:
        if len(sys.argv) > 4:
            os.write(int(sys.argv[4]), b'ready\n')
        (root / ('ready-' + str(index))).write_text(str(os.getpid()))
        with (root / ('heartbeat-' + str(index))).open('ab', buffering=0) as output:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline and not (root / 'release').exists():
                output.write(b'x')
                time.sleep(0.01)
        os._exit(0)
    children.append(child)
while not all((root / ('ready-' + str(index))).exists() for index in range(count)):
    time.sleep(0.002)
(root / 'root-ready').write_text(str(os.getpid()))
if mode == 'exit':
    os._exit(0)
while not (root / 'release').exists():
    time.sleep(0.01)
os._exit(0)
";

struct Fixture {
    root: PathBuf,
    group: Option<OwnedCommandGroup>,
}

impl Fixture {
    fn start(mode: &str, count: usize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "lean-owned-group-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let mut command = Self::command(&root, mode, count);
        command.stdout(File::create(root.join("stdout")).unwrap());
        let group = OwnedCommandGroup::spawn(command).unwrap();
        let fixture = Self {
            root,
            group: Some(group),
        };
        wait_until(|| fixture.root.join("root-ready").exists());
        fixture
    }

    fn command(root: &std::path::Path, mode: &str, count: usize) -> Command {
        let mut command = Command::new("python3");
        command
            .args(["-c", SCRIPT])
            .arg(root)
            .arg(mode)
            .arg(count.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn group(&mut self) -> &mut OwnedCommandGroup {
        self.group.as_mut().unwrap()
    }

    fn heartbeat(&self) -> u64 {
        fs::metadata(self.root.join("heartbeat-0")).map_or(0, |metadata| metadata.len())
    }

    fn root_exited(&mut self) {
        wait_until(|| macos::peek_exit(self.group().id()).unwrap());
    }

    fn finish(&mut self) -> ExitStatus {
        let mut status = None;
        wait_until(|| {
            status = self.group().try_wait().unwrap();
            status.is_some()
        });
        status.unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(group) = &mut self.group {
            let _stop = group.request_stop();
            let deadline = Instant::now() + LIMIT;
            while Instant::now() < deadline {
                if matches!(group.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let _removed = fs::remove_dir_all(&self.root);
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < LIMIT, "private process fixture did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn exited_root_keeps_redirected_writer_pending_until_explicit_stop() {
    let mut fixture = Fixture::start("exit", 1);
    fixture.root_exited();
    assert!(fixture.group().try_wait().unwrap().is_none());
    assert!(fixture.group().cleanup_pending());
    assert!(!fixture.group().intervened());
    let before = fixture.heartbeat();
    wait_until(|| fixture.heartbeat() > before);
    assert!(fixture.group().try_wait().unwrap().is_none());
    fixture.group().request_stop().unwrap();
    let status = fixture.finish();
    assert_eq!(
        status.code(),
        Some(0),
        "retain actual root exit, not descendant kill status"
    );
    assert!(fixture.group().intervened());
    assert!(!fixture.group().cleanup_pending());
    assert_eq!(fixture.group().try_wait().unwrap(), Some(status));
    let stopped = fixture.heartbeat();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(fixture.heartbeat(), stopped);
}

#[test]
fn descendants_can_finish_naturally_without_intervention() {
    let mut fixture = Fixture::start("exit", 1);
    fixture.root_exited();
    assert!(fixture.group().try_wait().unwrap().is_none());
    fs::write(fixture.root.join("release"), b"finish").unwrap();
    assert_eq!(fixture.finish().code(), Some(0));
    assert!(!fixture.group().intervened());
    assert!(!fixture.group().cleanup_pending());
}

#[test]
fn explicit_stop_kills_a_term_ignoring_private_group() {
    use rustix::process::{Pid, Signal, kill_process_group};
    let mut fixture = Fixture::start("live", 1);
    let pid = i32::try_from(fixture.group().id()).unwrap();
    kill_process_group(Pid::from_raw(pid).unwrap(), Signal::TERM).unwrap();
    let before = fixture.heartbeat();
    wait_until(|| fixture.heartbeat() > before);
    assert!(fixture.group().try_wait().unwrap().is_none());
    fixture.group().request_stop().unwrap();
    assert!(!fixture.finish().success());
    assert!(fixture.group().intervened());
    assert!(!fixture.group().cleanup_pending());
}

#[test]
fn stopping_one_group_leaves_an_unrelated_private_group_running() {
    let mut stopped = Fixture::start("exit", 1);
    let mut unrelated = Fixture::start("live", 1);
    stopped.root_exited();
    stopped.group().request_stop().unwrap();
    assert_eq!(stopped.finish().code(), Some(0));
    let before = unrelated.heartbeat();
    wait_until(|| unrelated.heartbeat() > before);
    assert!(unrelated.group().try_wait().unwrap().is_none());
    assert!(!unrelated.group().intervened());
    unrelated.group().request_stop().unwrap();
    unrelated.finish();
}

#[test]
fn full_two_pid_snapshot_never_reaps_a_three_member_group() {
    let mut fixture = Fixture::start("exit", 2);
    fixture.root_exited();
    assert_eq!(macos::members(fixture.group().id()).unwrap(), macos::Members::Others);
    assert!(fixture.group().try_wait().unwrap().is_none());
    assert!(fixture.root.join("ready-1").exists());
    fixture.group().request_stop().unwrap();
    assert_eq!(fixture.finish().code(), Some(0));
}

#[test]
fn inherited_pipe_stays_open_after_root_exit_until_group_closes() {
    let mut fixture = Fixture::start("exit", 0);
    fixture.finish();
    let (mut observer, inherited) = UnixStream::pair().unwrap();
    observer.set_read_timeout(Some(LIMIT)).unwrap();
    let mut command = Fixture::command(&fixture.root, "exit", 1);
    command.arg(inherited.as_raw_fd().to_string());
    fixture.group = Some(OwnedCommandGroup::spawn_inheriting(command, &[inherited.as_fd()]).unwrap());
    drop(inherited);
    let mut bytes = [0_u8; 6];
    observer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"ready\n");
    fixture.root_exited();
    assert!(fixture.group().try_wait().unwrap().is_none());
    observer.set_nonblocking(true).unwrap();
    assert_eq!(observer.read(&mut bytes).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    fixture.group().request_stop().unwrap();
    assert_eq!(fixture.finish().code(), Some(0));
    wait_until(|| matches!(observer.read(&mut bytes), Ok(0)));
}

#[test]
fn inherited_borrowed_file_is_closed_only_in_the_child_and_parent_keeps_cloexec() {
    use rustix::io::{FdFlags, fcntl_getfd};
    let mut fixture = Fixture::start("exit", 0);
    fixture.finish();
    let marker = fixture.root.join("ownership-file");
    fs::write(&marker, b"ownership").unwrap();
    let file = File::open(&marker).unwrap();
    assert!(fcntl_getfd(&file).unwrap().contains(FdFlags::CLOEXEC));
    let mut command = Command::new("python3");
    command
        .args([
            "-c",
            "import os, sys; fd = int(sys.argv[1]); os.fstat(fd); os.close(fd)",
        ])
        .arg(file.as_raw_fd().to_string());
    fixture.group = Some(OwnedCommandGroup::spawn_inheriting(command, &[file.as_fd()]).unwrap());
    assert!(fixture.finish().success());
    assert!(fcntl_getfd(&file).unwrap().contains(FdFlags::CLOEXEC));
    assert_eq!(file.metadata().unwrap().len(), 9);
    assert_eq!(fs::read(marker).unwrap(), b"ownership");
}

#[test]
fn observation_failure_keeps_cleanup_pending_without_signaling() {
    let mut fixture = Fixture::start("exit", 1);
    fixture.root_exited();
    fixture.group().stop_requested = true;
    let mut signaled = false;
    let result = fixture.group().poll_macos_with(
        macos::peek_exit,
        |_| Err(io::Error::from_raw_os_error(libc::EIO)),
        |_| {
            signaled = true;
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EIO));
    assert!(!signaled);
    assert!(fixture.group().cleanup_pending());
    assert!(fixture.group().intervened());
    let before = fixture.heartbeat();
    wait_until(|| fixture.heartbeat() > before);
    fixture.group().request_stop().unwrap();
    assert_eq!(fixture.finish().code(), Some(0));
}

#[test]
fn failed_kill_keeps_exact_group_owned_for_later_retry() {
    let mut fixture = Fixture::start("live", 1);
    fixture.group().stop_requested = true;
    fixture.group().cleanup_pending = true;
    let result = fixture.group().poll_macos_with(macos::peek_exit, macos::members, |_| {
        Err(io::Error::from_raw_os_error(libc::EIO))
    });
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EIO));
    assert!(fixture.group().cleanup_pending());
    let before = fixture.heartbeat();
    wait_until(|| fixture.heartbeat() > before);
    fixture.group().request_stop().unwrap();
    assert!(!fixture.finish().success());
}

#[test]
fn permission_failure_is_pending_and_never_a_closure_witness() {
    let mut fixture = Fixture::start("live", 1);
    fixture.group().stop_requested = true;
    fixture.group().cleanup_pending = true;
    assert!(
        fixture
            .group()
            .poll_macos_with(macos::peek_exit, macos::members, |_| Err(io::Error::from_raw_os_error(
                libc::EPERM
            )),)
            .unwrap()
            .is_none()
    );
    assert!(fixture.group().cleanup_pending());
    let before = fixture.heartbeat();
    wait_until(|| fixture.heartbeat() > before);
    fixture.group().request_stop().unwrap();
    assert!(!fixture.finish().success());
}

#[test]
fn successful_signal_delivery_does_not_itself_confirm_closure() {
    let mut fixture = Fixture::start("live", 1);
    fixture.group().stop_requested = true;
    let mut signaled = false;
    assert!(
        fixture
            .group()
            .poll_macos_with(macos::peek_exit, macos::members, |_| {
                signaled = true;
                Ok(())
            },)
            .unwrap()
            .is_none()
    );
    assert!(signaled);
    assert!(fixture.group().cleanup_pending());
    fixture.group().request_stop().unwrap();
    assert!(!fixture.finish().success());
}

#[test]
fn echild_permanently_revokes_signaling_even_if_pid_could_be_reused() {
    let mut command = Command::new("/usr/bin/true");
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut group = OwnedCommandGroup::spawn(command).unwrap();
    // This test alone deliberately breaks the sole-waiter contract. The actual
    // short child is already reaped before injecting ECHILD; no live PID needs cleanup.
    assert!(group.child.wait().unwrap().success());
    let mut observed = false;
    let mut signaled = false;
    let result = group.poll_macos_with(
        |_| Err(io::Error::from_raw_os_error(libc::ECHILD)),
        |_| {
            observed = true;
            Ok(macos::Members::Others)
        },
        |_| {
            signaled = true;
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ECHILD));
    assert!(!observed && !signaled);
    assert!(group.cleanup_pending());
    assert_eq!(group.request_stop().unwrap_err().raw_os_error(), Some(libc::ECHILD));
    assert_eq!(group.try_wait().unwrap_err().raw_os_error(), Some(libc::ECHILD));
    assert!(!observed && !signaled);
}

#[test]
fn group_decoder_accepts_only_a_complete_exact_single_leader() {
    let bytes = i32::try_from(std::mem::size_of::<libc::pid_t>()).unwrap();
    assert_eq!(
        macos::decode_members(42, [42, 0], bytes, 0).unwrap(),
        macos::Members::OnlyLeader
    );
    for (pids, count) in [
        ([42, 0], 0),
        ([42, 0], -1),
        ([42, 0], 1),
        ([42, 0], bytes - 1),
        ([41, 0], bytes),
        ([-1, 0], bytes),
        ([0, 0], bytes),
        ([42, 43], bytes * 3),
    ] {
        assert_eq!(
            macos::decode_members(42, pids, count, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    for pids in [[42, 43], [42, 0], [0, 0]] {
        assert_eq!(
            macos::decode_members(42, pids, bytes * 2, 0).unwrap(),
            macos::Members::Others
        );
    }
    for count in [-1, 0, bytes, bytes * 2] {
        assert_eq!(
            macos::decode_members(42, [42, 0], count, libc::EACCES)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
    }
}

#[test]
fn passive_group_presence_uses_the_same_real_scope_boundary() {
    let mut fixture = Fixture::start("run", 1);
    let group = fixture.group().id();
    assert!(super::group_present(group).unwrap());
    fixture.group().request_stop().unwrap();
    fixture.finish();
    assert!(!super::group_present(group).unwrap());
}

#[test]
fn passive_group_presence_decoder_rejects_failures_and_malformed_data() {
    assert!(!macos::decode_presence([0, 0], 0, 0).unwrap());
    assert!(macos::decode_presence([42, 0], 4, 0).unwrap());
    assert!(macos::decode_presence([42, 43], 8, 0).unwrap());
    for (pids, count) in [([0, 0], 4), ([42, 0], 8), ([42, 0], 3), ([42, 43], 12), ([42, 0], -1)] {
        assert!(macos::decode_presence(pids, count, 0).is_err());
    }
    for count in [0, 4, 8] {
        assert_eq!(
            macos::decode_presence([42, 43], count, libc::EPERM)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
    }
    assert!(super::group_present(0).is_err());
    assert!(super::group_present(1).is_err());
    assert!(super::group_present(u32::MAX).is_err());
}
