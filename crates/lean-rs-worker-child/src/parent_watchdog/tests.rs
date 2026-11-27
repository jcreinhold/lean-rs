use super::*;
use std::fs;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn std::error::Error>>;

static NEXT: AtomicUsize = AtomicUsize::new(0);
const DIRECTORY_ENV: &str = "LEAN_WATCHDOG_TEST_DIRECTORY";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> io::Result<Self> {
        let directory = std::env::temp_dir().join(format!(
            "lean-watchdog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory)?;
        Ok(Self(directory))
    }
    fn command(&self, lifeline: &str) -> io::Result<Command> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "parent_watchdog::tests::child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env(DIRECTORY_ENV, &self.0)
            .env(LIFELINE_ENV, lifeline)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        Ok(command)
    }
    fn reached_body(&self) -> bool {
        self.0.join("body").exists()
    }
    fn refusal(&self) -> io::Result<String> {
        fs::read_to_string(self.0.join("refused"))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _cleanup = fs::remove_dir_all(&self.0);
    }
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> io::Result<bool>) -> io::Result<bool> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if done()? {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

fn wait_for_exit(child: &mut Child, limit: Duration) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let mut status = None;
    if !wait_until(limit, || {
        status = child.try_wait()?;
        Ok(status.is_some())
    })? {
        // Only this still-owned, unreaped Child is terminated.
        child.kill()?;
        child.wait()?;
        return Err("fixture child did not exit in time".into());
    }
    status.ok_or_else(|| "fixture child status missing".into())
}

/// Stands in for a worker: it installs the lifeline, then blocks its main
/// thread the way a long Lean call does.
#[test]
#[ignore = "internal lifeline harness; the parent test starts it in a child process"]
fn child_fixture() -> TestResult {
    let Some(directory) = std::env::var_os(DIRECTORY_ENV).map(PathBuf::from) else {
        return Ok(());
    };
    if let Err(error) = install_parent_watchdog() {
        fs::write(directory.join("refused"), error.to_string())?;
        return Ok(());
    }
    fs::write(directory.join("body"), "started")?;
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn busy_child_exits_when_the_parent_closes_its_lifeline() -> TestResult {
    let fixture = Fixture::new()?;
    let (reader, writer) = std::io::pipe()?;
    let command = fixture.command(&reader.as_raw_fd().to_string())?;
    let mut child = lean_toolchain::process_group::spawn_inheriting(command, &[reader.as_fd()])?;
    drop(reader);
    if !wait_until(Duration::from_secs(10), || Ok(fixture.reached_body()))? {
        child.kill()?;
        child.wait()?;
        return Err(format!("fixture never started: {:?}", fixture.refusal()).into());
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(child.try_wait()?.is_none(), "child exited while the lifeline was open");
    drop(writer);
    assert_eq!(wait_for_exit(&mut child, Duration::from_secs(3))?.code(), Some(125));
    Ok(())
}

#[test]
fn invalid_lifelines_stop_startup_before_the_body() -> TestResult {
    let (_reader, writer) = std::io::pipe()?;
    let regular = fs::File::open(std::env::current_exe()?)?;
    let cases: [(String, Option<std::os::fd::BorrowedFd<'_>>); 5] = [
        ("not-a-number".to_owned(), None),
        ("2".to_owned(), None),
        ("1000000".to_owned(), None),
        (regular.as_raw_fd().to_string(), Some(regular.as_fd())),
        (writer.as_raw_fd().to_string(), Some(writer.as_fd())),
    ];
    for (value, inherited) in cases {
        let fixture = Fixture::new()?;
        let command = fixture.command(&value)?;
        let mut child = lean_toolchain::process_group::spawn_inheriting(command, inherited.as_slice())?;
        let status = wait_for_exit(&mut child, Duration::from_secs(10))?;
        assert!(status.success(), "fixture for {value:?} failed: {status}");
        assert!(!fixture.reached_body(), "{value:?} reached the body");
        assert!(!fixture.refusal()?.is_empty(), "{value:?} gave no reason");
    }
    Ok(())
}
