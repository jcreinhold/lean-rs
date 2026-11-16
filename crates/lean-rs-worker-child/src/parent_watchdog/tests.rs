use super::*;
use std::fs;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static NEXT: AtomicUsize = AtomicUsize::new(0);
const NONCE: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdef";

fn wait_for(mut test: impl FnMut() -> bool) {
    let started = Instant::now();
    while !test() {
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fixture(group_leader: bool, nonce: &str) -> (PathBuf, UnixListener, Child) {
    let directory = PathBuf::from("/tmp").join(format!(
        "lean-watchdog-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    let listener = UnixListener::bind(directory.join("parent.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "parent_watchdog::tests::child_fixture", "--nocapture"])
        .env("LEAN_WATCHDOG_TEST_DIRECTORY", &directory)
        .env("LEAN_RS_WORKER_PARENT_SOCKET", directory.join("parent.sock"))
        .env("LEAN_RS_WORKER_PARENT_NONCE", nonce);
    if group_leader {
        command.process_group(0);
    }
    (directory, listener, command.spawn().unwrap())
}

#[allow(
    clippy::panic,
    reason = "an isolated fixture must fail immediately on an unexpected local socket error"
)]
fn connected(listener: &UnixListener) -> UnixStream {
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(started.elapsed() < Duration::from_secs(3));
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fixture accept failed: {error}"),
        }
    }
}

#[test]
fn child_fixture() {
    let Some(directory) = std::env::var_os("LEAN_WATCHDOG_TEST_DIRECTORY") else {
        return;
    };
    if install_parent_watchdog().is_err() {
        return;
    }
    fs::write(Path::new(&directory).join("body"), "once").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn actual_bootstrap_waits_for_ack_then_exits_only_self_on_eof() {
    let (directory, listener, mut child) = fixture(false, NONCE);
    let mut stream = connected(&listener);
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut hello = String::new();
    BufReader::new(&stream).read_line(&mut hello).unwrap();
    assert_eq!(hello, format!("{NONCE} {}\n", child.id()));
    assert!(!directory.join("body").exists());
    stream.write_all(b"durable-owner-ready\n").unwrap();
    wait_for(|| directory.join("body").exists());
    drop(stream);
    let mut exit = None;
    wait_for(|| {
        exit = child.try_wait().unwrap();
        exit.is_some()
    });
    assert_eq!(exit.unwrap().code(), Some(125));
    assert_eq!(fs::read_to_string(directory.join("body")).unwrap(), "once");
    drop(listener);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn no_ack_never_reaches_runtime_body() {
    let (directory, listener, mut child) = fixture(false, NONCE);
    let mut stream = connected(&listener);
    let mut hello = String::new();
    BufReader::new(&mut stream).read_line(&mut hello).unwrap();
    drop(stream);
    wait_for(|| child.try_wait().unwrap().is_some());
    assert!(!directory.join("body").exists());
    drop(listener);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn invalid_nonce_and_failed_setsid_never_reach_runtime_body() {
    for (group_leader, nonce) in [(false, "bad-nonce"), (true, NONCE)] {
        let (directory, listener, mut child) = fixture(group_leader, nonce);
        wait_for(|| child.try_wait().unwrap().is_some());
        assert!(!directory.join("body").exists());
        drop(listener);
        fs::remove_dir_all(directory).unwrap();
    }
}
