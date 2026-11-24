//! Direct SDK decoder regressions, independent of the production H producer.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "private protocol fixtures assert preserved closure evidence and rejected result bytes"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use lean_toolchain::shared_writer::{SharedWriter, WriterDisposition, WriterFailure};

const LIMIT: Duration = Duration::from_secs(2);
const FAKE_HELPER: &str = r"#!/usr/bin/env python3
import json, os, select, shutil, signal, subprocess, sys
from pathlib import Path

# Bound even a failed decoder's fixture lifetime; this helper runs no Lean.
signal.signal(signal.SIGALRM, lambda *args: sys.exit(124))
signal.alarm(3)
root = Path(sys.argv[2])
mode = (root / 'mode').read_text()
assets = root / 'assets'
assets.mkdir(mode=0o700)
owner = {'version': 1, 'pid': os.getpid(), 'session': os.getpid(), 'start': 'private-sdk-fixture'}
loans = json.loads(os.environ['LEAN_RS_WRITER_LOCK_LOANS'])
loans = [{key: loan[key] for key in ('path', 'device', 'inode')} for loan in loans]
input = sys.stdin.buffer.raw

def send(frame):
    print(json.dumps(frame), flush=True)

send({'state': 'locked', 'owner': owner, 'asset_dir': str(assets), 'lock_loans': loans})
message = json.loads(input.readline())
with (root / 'runs').open('a') as log:
    log.write('run\n')
child = subprocess.Popen(message['command'], cwd=message['cwd'], stdin=subprocess.DEVNULL,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
stdout, stderr = child.communicate(timeout=1)
process = {'pid': child.pid, 'process_group': child.pid, 'session': owner['session'],
           'start': 'private-actual-child', 'start_new_session': False}
if mode != 'stopped_before_started':
    send({'state': 'started', 'owner': owner, 'process': process})
    (root / 'started_sent').write_text('sent')
frame = {'state': 'stopped', 'owner': owner, 'process': process, 'owned_work_stopped': True,
         'exit_code': child.returncode, 'stdout': stdout.decode(), 'stderr': stderr.decode()}
if mode == 'stopped_before_started':
    frame['result'] = 'no_result'
elif mode == 'cancelled_string':
    frame['cancelled'] = 'true'
elif mode == 'intervened_string':
    frame['intervened'] = 'true'
elif mode == 'result_unknown':
    frame['result'] = 'unknownkind'
send(frame)
# A failed result must seal this SDK lifetime rather than launch another stage.
while select.select([input], [], [], 2)[0]:
    line = input.readline()
    if not line:
        break
    if json.loads(line).get('action') == 'run':
        with (root / 'runs').open('a') as log:
            log.write('run\n')
shutil.rmtree(assets)
(root / 'closed').write_text('actual child already exited; helper custody closed')
";

fn wait_for(path: &Path) {
    let started = Instant::now();
    while !path.is_file() {
        assert!(started.elapsed() < LIMIT, "private helper did not close");
        thread::sleep(Duration::from_millis(5));
    }
}

fn rejected_terminal(mode: &str) -> WriterFailure {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let executable = root.join("fake-sdk-helper");
    fs::write(&executable, FAKE_HELPER).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("mode"), mode).unwrap();
    let mut writer = SharedWriter::open(&executable, &root, LIMIT).unwrap();
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg("printf '{\"module\":\"Demo\",\"options\":{}}'");
    let error = writer
        .run(&command, LIMIT)
        .expect_err("failed terminal metadata must never become Output");
    let failure = WriterFailure::from_io(&error).expect("typed SDK evidence").clone();
    assert_eq!(failure.disposition, WriterDisposition::Stopped);
    let receipt = failure
        .stop_receipt
        .as_ref()
        .expect("validated closure receipt survives failed result");
    assert_eq!(receipt["state"], "stopped");
    assert_eq!(receipt["owned_work_stopped"], true);
    assert_eq!(
        receipt["exit_code"], 0,
        "exit zero does not repair unavailable result bytes"
    );
    assert_eq!(receipt["stdout"], "{\"module\":\"Demo\",\"options\":{}}");
    assert_eq!(root.join("started_sent").exists(), mode != "stopped_before_started");

    let replay = writer
        .run(&command, LIMIT)
        .expect_err("failed lifetime cannot launch another stage");
    assert_eq!(
        WriterFailure::from_io(&replay).unwrap().disposition,
        WriterDisposition::Unknown
    );
    drop(writer);
    wait_for(&root.join("closed"));
    assert_eq!(fs::read_to_string(root.join("runs")).unwrap(), "run\n");
    failure
}

#[test]
fn sdk_retains_stopped_no_result_without_a_prior_started_frame() {
    let failure = rejected_terminal("stopped_before_started");
    assert_eq!(failure.stop_receipt.unwrap()["result"], "no_result");
}

#[test]
fn sdk_rejects_malformed_optional_terminal_flags_despite_exit_zero() {
    for (mode, field, malformed) in [
        ("cancelled_string", "cancelled", "true"),
        ("intervened_string", "intervened", "true"),
        ("result_unknown", "result", "unknownkind"),
    ] {
        let failure = rejected_terminal(mode);
        assert_eq!(failure.stop_receipt.unwrap()[field], malformed);
    }
}
