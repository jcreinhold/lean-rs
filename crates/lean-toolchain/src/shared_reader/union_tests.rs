//! Whole-domain admission regressions with real private helper processes.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "isolated real-process fixtures assert exact admission and descriptor outcomes"
)]
use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, Ordering};

const WAIT: Duration = Duration::from_secs(4);
const SCRIPT: &str = r"
import array, errno, fcntl, json, os, pathlib, select, signal, socket, sys, time
BASE = pathlib.Path(CONFIG['base'])
ARGS = sys.argv[1:]
report = '--report-contention' in ARGS
if report: ARGS.remove('--report-contention')
kind, root, lock, admission, endpoint, nonce = ARGS
# Reader transfers use SOCKET,NONCE; proxy custody uses NONCE,FIFO.
if kind == 'read-proxy': endpoint, nonce = nonce, endpoint
root = pathlib.Path(root)
mode = CONFIG['mode']
def event(name):
    row = json.dumps({'event':name,'root':str(root),'report':report,'pid':os.getpid()})+'\n'
    fd=os.open(BASE/'events.jsonl', os.O_WRONLY|os.O_CREAT|os.O_APPEND, 0o600)
    try: os.write(fd,row.encode())
    finally: os.close(fd)
def wait_release(name, seconds=3):
    until=time.monotonic()+seconds
    while not (BASE/name).exists():
        if time.monotonic()>=until: return False
        time.sleep(.002)
    return True
event('start')
if mode=='always_busy':
    sys.stderr.write('explicit busy fixture\n'); sys.exit(75)
if mode=='hard':
    sys.stderr.write('hard-sentinel: busy is diagnostic, not authority\n'+'x'*70000)
    sys.stderr.flush(); sys.exit(7)
if mode=='signal':
    sys.stderr.write('busy is diagnostic\n'); sys.stderr.flush(); os.kill(os.getpid(),signal.SIGTERM)
if mode=='hang_accept':
    wait_release('release',2);sys.exit(0)
if mode=='delay': time.sleep(.12)
fd=os.open(lock, os.O_RDWR|os.O_CREAT,0o600)
try: fcntl.flock(fd,fcntl.LOCK_SH|fcntl.LOCK_NB)
except BlockingIOError:
    event('busy');sys.stderr.write('fixture artifact lock busy\n');sys.exit(75 if report else 1)
identity=os.fstat(fd)
frame={'protocol':1,'state':'read_ready','project_root':str(root),'nonce':nonce,'epoch':'fixture-epoch',
       'lock_identity':{'device':identity.st_dev,'inode':identity.st_ino}}
if mode=='wrong_nonce': frame['nonce']='wrong-nonce'
if kind=='reader':
    conn=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);conn.connect(endpoint);event('connected')
    if mode=='connect75': conn.close();sys.exit(75)
    if mode=='hang_receive':
        conn.sendall(b'{');event('partial');wait_release('release',2);sys.exit(0)
    if mode=='partial_fd75':
        conn.sendmsg([b'{'],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,array.array('i',[fd]))]);conn.close();sys.exit(75)
    payload=(json.dumps(frame)+'\n').encode()
    if mode=='malformed': payload=b'{}\n'
    descriptors=[fd,fd] if mode=='extra_fd' else [fd]
    conn.sendmsg([payload],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,array.array('i',descriptors))])
    event('ready');conn.close();os.close(fd)
    sys.exit(75 if mode=='fd75' else 0)
if kind!='read-proxy': raise SystemExit('unknown helper mode')
if mode=='proxy_byte75':
    sys.stdout.buffer.write(b'x');sys.stdout.buffer.flush();sys.exit(75)
frame['custody']='proxy'
control=os.open(endpoint,os.O_RDONLY|os.O_NONBLOCK)
sys.stdout.write(json.dumps(frame)+'\n');sys.stdout.flush();event('ready')
if mode=='proxy_ready75': sys.exit(75)
until=time.monotonic()+5
while time.monotonic()<until:
    try:
        if os.read(control,1024)==b'': break
    except BlockingIOError: pass
    time.sleep(.002)
event('eof')
if mode=='proxy_delay_cleanup' and str(root)==CONFIG['early']:
    event('cleanup_wait');wait_release('cleanup.release',2)
os.close(control);os.close(fd);event('closed')
";

struct Fixture {
    directory: SocketDirectory,
    roots: Vec<PathBuf>,
    locks: Vec<PathBuf>,
    helper: PathBuf,
}
impl Fixture {
    fn new(mode: &str) -> io::Result<Self> {
        let (directory, _) = SocketDirectory::new(None)?;
        let mut keyed = ["one", "two"]
            .into_iter()
            .map(|name| {
                let root = directory.0.join(name);
                fs::create_dir(&root)?;
                Ok((crate::shared_writer::project_lock_path(&root)?, root))
            })
            .collect::<io::Result<Vec<_>>>()?;
        keyed.sort();
        let locks = keyed.iter().map(|(lock, _)| lock.clone()).collect();
        let roots: Vec<_> = keyed.into_iter().map(|(_, root)| root).collect();
        let helper = directory.0.join("helper.py");
        let config = json!({"base":directory.0,"mode":mode,"early":roots[0]});
        let encoded = serde_json::to_string(&config.to_string())?;
        fs::write(
            &helper,
            format!("#!/usr/bin/env python3\nimport json\nCONFIG=json.loads({encoded})\n{SCRIPT}"),
        )?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            directory,
            roots,
            locks,
            helper,
        })
    }
    fn base(&self) -> &Path {
        &self.directory.0
    }
    fn events(&self) -> io::Result<Vec<Value>> {
        match fs::read_to_string(self.base().join("events.jsonl")) {
            Ok(rows) => rows
                .lines()
                .map(|line| serde_json::from_str(line).map_err(io::Error::other))
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
    fn starts(&self) -> io::Result<usize> {
        Ok(self.events()?.iter().filter(|row| row["event"] == "start").count())
    }
    fn observed(&self, event: &str) -> bool {
        self.events()
            .is_ok_and(|rows| rows.iter().any(|row| row["event"] == event))
    }
    fn lock(&self, index: usize) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.locks[index])
    }
    fn release(&self, name: &str) -> io::Result<()> {
        fs::write(self.base().join(name), b"release")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _release = self.release("release");
        let _cleanup = self.release("cleanup.release");
        // Every delayed fixture helper has a finite self-exit backstop. The SDK
        // remains its sole wait owner; this fixture never signals a saved PID.
    }
}

struct Writer(Child);
impl Writer {
    fn start(lock: &Path, ready: &Path, release: &Path) -> io::Result<Self> {
        Self::start_after(lock, ready, release, None)
    }
    fn start_after(lock: &Path, ready: &Path, release: &Path, begin: Option<&Path>) -> io::Result<Self> {
        let script = r"import fcntl,os,pathlib,sys,time
lock,ready,release=map(pathlib.Path,sys.argv[1:4]);fd=os.open(lock,os.O_RDWR|os.O_CREAT,0o600)
end=time.monotonic()+4
if sys.argv[4]!='-':
 begin=pathlib.Path(sys.argv[4])
 while not begin.exists():
  if time.monotonic()>=end: raise SystemExit(5)
  time.sleep(.002)
while True:
 try: fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);break
 except BlockingIOError:
  if time.monotonic()>=end: raise SystemExit(3)
  time.sleep(.002)
ready.write_text('EX')
while not release.exists():
 if time.monotonic()>=end: raise SystemExit(4)
 time.sleep(.002)
os.close(fd)
";
        Ok(Self(
            Command::new("python3")
                .args(["-c", script])
                .arg(lock)
                .arg(ready)
                .arg(release)
                .arg(begin.map_or_else(|| std::ffi::OsString::from("-"), |path| path.as_os_str().to_owned()))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        ))
    }
    fn finish(&mut self) -> io::Result<()> {
        let deadline = Instant::now().checked_add(WAIT).ok_or_else(timed_out)?;
        loop {
            if let Some(status) = self.0.try_wait()? {
                assert!(status.success(), "private writer: {status}");
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(timed_out());
            }
            thread::sleep(POLL);
        }
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _kill = self.0.kill();
        }
        let _wait = self.0.wait();
    }
}
fn wait_for(mut ready: impl FnMut() -> bool) -> io::Result<()> {
    let deadline = Instant::now().checked_add(WAIT).ok_or_else(timed_out)?;
    while !ready() {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        thread::sleep(POLL);
    }
    Ok(())
}
fn union(proxy: bool, fixture: &Fixture, timeout: Duration, cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if proxy {
        drop(SharedReadProxy::acquire_union(
            &fixture.helper,
            &fixture.roots,
            timeout,
            cancelled,
        )?);
    } else {
        drop(SharedReader::acquire_union(
            &fixture.helper,
            &fixture.roots,
            timeout,
            cancelled,
        )?);
    }
    Ok(())
}

#[test]
fn late_domain_contention_releases_all_earlier_reads_before_waiting() -> io::Result<()> {
    for proxy in [false, true] {
        let fixture = Fixture::new("normal")?;
        let late_ready = fixture.base().join("late.ready");
        let late_release = fixture.base().join("late.release");
        let mut late = Writer::start(&fixture.locks[1], &late_ready, &late_release)?;
        wait_for(|| late_ready.exists())?;
        let early_ready = fixture.base().join("early.ready");
        let early_release = fixture.base().join("early.release");
        let early_begin = fixture.base().join("early.begin");
        // Spawn every fixture child before Darwin SCM_RIGHTS receiving starts.
        let mut early = Writer::start_after(&fixture.locks[0], &early_ready, &early_release, Some(&early_begin))?;
        let helper = fixture.helper.clone();
        let roots = fixture.roots.clone();
        let (send, receive) = mpsc::channel();
        let thread = thread::spawn(move || {
            let result = if proxy {
                SharedReadProxy::acquire_union(&helper, &roots, WAIT, &|| false).map(drop)
            } else {
                SharedReader::acquire_union(&helper, &roots, WAIT, &|| false).map(drop)
            };
            let _sent = send.send(result);
        });
        wait_for(|| fixture.observed("busy"))?;
        fs::write(&early_begin, b"attempt EX after tentative SH")?;
        wait_for(|| early_ready.exists())?;
        assert!(
            !late_release.exists(),
            "earlier writer must progress before late writer releases"
        );
        assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
        fs::write(&early_release, b"close")?;
        early.finish()?;
        fs::write(&late_release, b"close")?;
        late.finish()?;
        receive.recv_timeout(WAIT).map_err(io::Error::other)??;
        thread.join().map_err(|_| invalid("union owner panicked"))?;
        assert!(fixture.starts()? > 2, "typed contention should retry only admission");
    }
    Ok(())
}

#[test]
fn fixed_root_order_is_preserved_after_lock_ordered_admission() -> io::Result<()> {
    let fixture = Fixture::new("normal")?;
    let roots = fixture.roots.iter().rev().cloned().collect::<Vec<_>>();
    let readers = SharedReader::acquire_union(&fixture.helper, &roots, WAIT, &|| false)?;
    assert_eq!(readers.len(), roots.len());
    for (reader, root) in readers.iter().zip(&roots) {
        reader.guardian_loan(root)?;
    }
    drop(readers);
    Ok(())
}

#[test]
fn hard_exit_or_signal_is_not_retried_even_when_stderr_says_busy() -> io::Result<()> {
    for mode in ["hard", "signal"] {
        for proxy in [false, true] {
            let fixture = Fixture::new(mode)?;
            let started = Instant::now();
            let error = union(proxy, &fixture, WAIT, &|| false).expect_err("non-contention helper failure is terminal");
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "bounded stderr drain must not block helper exit"
            );
            assert_ne!(error.kind(), io::ErrorKind::TimedOut);
            assert!(!is_admission_contention(&error));
            assert_eq!(fixture.starts()?, 1, "hard helper was relaunched");
            let detail = error.to_string();
            assert!(detail.len() < 20 * 1024, "stderr diagnostic must remain bounded");
            assert!(
                detail.contains("hard-sentinel") || detail.contains("signal") || detail.contains("SIGTERM"),
                "missing helper diagnostic: {detail}"
            );
        }
    }
    Ok(())
}

#[test]
fn legacy_acquire_never_treats_exit75_as_retry_authority() -> io::Result<()> {
    for proxy in [false, true] {
        let fixture = Fixture::new("always_busy")?;
        let result = if proxy {
            SharedReadProxy::acquire(&fixture.helper, &fixture.roots[0], WAIT).map(drop)
        } else {
            SharedReader::acquire(&fixture.helper, &fixture.roots[0], WAIT).map(drop)
        };
        let error = result.expect_err("unflagged helper75 is hard");
        assert!(!is_admission_contention(&error));
        assert_eq!(fixture.starts()?, 1);
        assert!(fixture.events()?.iter().all(|row| row["report"] == false));
    }
    Ok(())
}

#[test]
fn any_connected_or_failed_descriptor_exchange_is_hard_even_with_exit75() -> io::Result<()> {
    for mode in [
        "connect75",
        "partial_fd75",
        "fd75",
        "wrong_nonce",
        "extra_fd",
        "malformed",
    ] {
        let fixture = Fixture::new(mode)?;
        let error = SharedReader::acquire_union(&fixture.helper, &fixture.roots, WAIT, &|| false)
            .expect_err("post-connection failure cannot authorize admission retry");
        assert!(
            !is_admission_contention(&error),
            "accepted a post-transfer busy marker for {mode}"
        );
        assert_eq!(fixture.starts()?, 1, "helper retried after {mode}");
    }
    Ok(())
}

#[test]
fn proxy_stdout_bytes_make_exit75_a_hard_failure() -> io::Result<()> {
    let fixture = Fixture::new("proxy_byte75")?;
    let error = SharedReadProxy::acquire_union(&fixture.helper, &fixture.roots, WAIT, &|| false)
        .map(drop)
        .expect_err("even partial stdout bytes invalidate pre-ready contention");
    assert!(!is_admission_contention(&error));
    assert_eq!(fixture.starts()?, 1);
    Ok(())
}

#[test]
fn cancellation_reaches_inner_accept_receive_and_proxy_polls() -> io::Result<()> {
    for (proxy, mode, event) in [
        (false, "hang_accept", "start"),
        (false, "hang_receive", "partial"),
        (true, "hang_accept", "start"),
    ] {
        let fixture = Fixture::new(mode)?;
        let started = Instant::now();
        let error = union(proxy, &fixture, WAIT, &|| fixture.observed(event))
            .expect_err("inner loop must observe cancellation without waiting full admission");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancellation consumed full helper budget"
        );
        assert_eq!(fixture.starts()?, 1);
    }
    Ok(())
}

#[test]
fn one_deadline_covers_busy_retries_and_all_root_helpers() -> io::Result<()> {
    for proxy in [false, true] {
        let fixture = Fixture::new("always_busy")?;
        let started = Instant::now();
        let error = union(proxy, &fixture, Duration::from_millis(120), &|| false).expect_err("busy admission deadline");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "contention retry reset original deadline"
        );
        assert!(fixture.starts()? >= 1);
        let delayed = Fixture::new("delay")?;
        let error = union(proxy, &delayed, Duration::from_millis(200), &|| false)
            .expect_err("each root may not receive a fresh full timeout");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
    Ok(())
}

#[test]
fn proxy_cleanup_uncertainty_prevents_any_next_admission_attempt() -> io::Result<()> {
    let fixture = Fixture::new("proxy_delay_cleanup")?;
    let late_ready = fixture.base().join("late.ready");
    let late_release = fixture.base().join("late.release");
    let mut late = Writer::start(&fixture.locks[1], &late_ready, &late_release)?;
    wait_for(|| late_ready.exists())?;
    let error = SharedReadProxy::acquire_union(&fixture.helper, &fixture.roots, Duration::from_millis(220), &|| false)
        .map(drop)
        .expect_err("unconfirmed partial proxy closure cannot authorize retry");
    assert!(!is_admission_contention(&error));
    assert!(fixture.observed("cleanup_wait"));
    assert_eq!(fixture.starts()?, 2, "retried while old SH cleanup unknown");
    assert!(
        fixture.lock(0)?.try_lock().is_err(),
        "partial helper should still hold SH in this fault fixture"
    );
    fixture.release("cleanup.release")?;
    wait_for(|| fixture.lock(0).is_ok_and(|file| file.try_lock().is_ok()))?;
    fs::write(late_release, b"close")?;
    late.finish()?;
    Ok(())
}

#[test]
fn pre_cancelled_union_launches_no_helper_and_try_union_makes_one_round() -> io::Result<()> {
    let fixture = Fixture::new("always_busy")?;
    let cancelled = AtomicBool::new(true);
    let error = SharedReader::acquire_union(&fixture.helper, &fixture.roots, WAIT, &|| {
        cancelled.load(Ordering::Acquire)
    })
    .expect_err("pre-cancel must not launch");
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(fixture.starts()?, 0);
    let error = SharedReader::try_acquire_union(&fixture.helper, &fixture.roots, WAIT, &|| false)
        .expect_err("one pre-body admission round");
    assert!(is_admission_contention(&error));
    assert_eq!(fixture.starts()?, 1);
    Ok(())
}

#[test]
fn proxy_readiness_then_exit75_never_authorizes_a_retry() -> io::Result<()> {
    let fixture = Fixture::new("proxy_ready75")?;
    let error = SharedReadProxy::acquire_union(&fixture.helper, &fixture.roots, WAIT, &|| false)
        .map(drop)
        .expect_err("a helper exit after readiness is a hard custody failure");
    assert!(!is_admission_contention(&error));
    let started = fixture.starts()?;
    assert!(
        (1..=2).contains(&started),
        "a post-ready helper exit restarted admission"
    );
    Ok(())
}

#[test]
fn complete_domain_preflight_refuses_demand_before_spawning_any_helper() -> io::Result<()> {
    let fixture = Fixture::new("normal")?;
    let demand = crate::writer_demand::WriterDemand::announce(&fixture.locks, WAIT, &|| false)?;
    let error = SharedReader::try_acquire_union(&fixture.helper, &fixture.roots, WAIT, &|| false)
        .expect_err("complete fixed domain demand is checked before any helper");
    assert!(is_admission_contention(&error));
    assert_eq!(fixture.starts()?, 0);
    drop(demand);
    let readers = SharedReader::try_acquire_union(&fixture.helper, &fixture.roots, WAIT, &|| false)?;
    assert_eq!(readers.len(), fixture.roots.len());
    assert_eq!(fixture.starts()?, 2);
    Ok(())
}
