#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "isolated child fixtures assert exact wait and accounting properties"
)]
use super::*;
use std::process::Stdio;
use std::time::Duration;
fn command(script: &str) -> Command {
    let mut command = Command::new("python3");
    command
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}
fn finish(child: &mut MeasuredChild) -> ExitStatus {
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "fixture failed to exit");
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn actual_wait_caches_exit_and_payload_usage() {
    let mut child = MeasuredChild::spawn(command("import time; x=bytearray(16*1024*1024); start=time.process_time();\nwhile time.process_time()-start<0.03: x[0]=1\nraise SystemExit(7)")).unwrap();
    assert!(child.usage().is_none());
    let status = finish(&mut child);
    assert_eq!(status.code(), Some(7));
    let usage = *child.usage().unwrap();
    assert!(usage.user_cpu_micros > 0);
    assert!(usage.wall_millis > 0);
    assert!(usage.max_rss_bytes.unwrap() >= 16 * 1024 * 1024);
    assert_eq!(child.try_wait().unwrap(), Some(status));
    assert_eq!(child.usage(), Some(&usage));
    child.kill().unwrap();
    assert_eq!(child.usage(), Some(&usage));
}
#[test]
fn unrelated_previously_waited_child_does_not_inflate_payload_peak() {
    let mut unrelated = command("x=bytearray(256*1024*1024)").spawn().unwrap();
    assert!(unrelated.wait().unwrap().success());
    let mut child = MeasuredChild::spawn(command("x=bytearray(1024*1024)")).unwrap();
    assert!(finish(&mut child).success());
    assert!(child.usage().unwrap().max_rss_bytes.unwrap() < 128 * 1024 * 1024);
}
#[test]
fn killed_payload_still_requires_and_records_actual_wait() {
    let mut child = MeasuredChild::spawn(command("import time; time.sleep(10)")).unwrap();
    assert!(child.try_wait().unwrap().is_none());
    assert!(child.usage().is_none());
    child.kill().unwrap();
    let status = finish(&mut child);
    assert!(!status.success());
    assert!(child.usage().is_some());
}
#[test]
#[allow(
    unsafe_code,
    reason = "fixture deliberately steals the exact unreaped child wait to verify authority revocation"
)]
fn lost_wait_authority_is_sticky_and_cannot_signal_another_child() {
    let mut child = MeasuredChild::spawn(command("raise SystemExit(0)")).unwrap();
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    let mut status = 0;
    // SAFETY: the fixture owns this exact Child and intentionally violates only
    // the documented sole-waiter contract; its PID is unreaped and still pinned.
    assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
    assert_eq!(child.try_wait().unwrap_err().raw_os_error(), Some(libc::ECHILD));
    assert!(child.usage().is_none());
    let mut companion = command("import time; time.sleep(10)").spawn().unwrap();
    assert_eq!(child.kill().unwrap_err().raw_os_error(), Some(libc::ECHILD));
    assert_eq!(child.try_wait().unwrap_err().raw_os_error(), Some(libc::ECHILD));
    assert!(companion.try_wait().unwrap().is_none());
    companion.kill().unwrap();
    companion.wait().unwrap();
}
