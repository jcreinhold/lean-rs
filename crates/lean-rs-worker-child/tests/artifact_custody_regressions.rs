#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Small native constructor fixtures; these never build or import Mathlib.
//! Opt in with `LEAN_TOOLCHAIN_READER_TEST_HELPER` and a canonical, private
//! `LEAN_RS_WORKER_OWNERS_DIRECTORY` (0700), then run this suite serially.
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lean_rs_worker_parent::{LeanWorker, LeanWorkerConfig, LeanWorkerSessionConfig};
use lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts;
use lean_toolchain::shared_writer::SharedWriter;

struct Fixture {
    root: PathBuf,
    helper: PathBuf,
    registry: PathBuf,
    descriptor: LeanWorkerPreparedHostArtifacts,
}
impl Fixture {
    fn new() -> Self {
        let helper = PathBuf::from(std::env::var_os("LEAN_TOOLCHAIN_READER_TEST_HELPER").expect("set actual H helper"));
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("lean-native-preload-{}-{nonce}", std::process::id()));
        fs::create_dir_all(root.join(".lake/build/lib/lean")).unwrap();
        let root = root.canonicalize().unwrap();
        let registry = PathBuf::from(
            std::env::var_os("LEAN_RS_WORKER_OWNERS_DIRECTORY")
                .expect("set a private LEAN_RS_WORKER_OWNERS_DIRECTORY for this isolated suite"),
        )
        .canonicalize()
        .unwrap();
        let source = root.join("constructor.c");
        fs::write(
            &source,
            r#"#include <stdio.h>
#include <stdlib.h>
__attribute__((constructor)) static void mark_load(void) {
  const char *path=getenv("LEAN_PRELOAD_MARKER");
  if(path) { FILE *f=fopen(path,"w"); if(f) { fputs("loaded",f); fclose(f); } }
}
void *initialize_lean__rs__host__shims_LeanRsHostShims(unsigned char builtin) { (void)builtin; return NULL; }
"#,
        )
        .unwrap();
        let library = root.join(if cfg!(target_os = "macos") {
            ".lake/build/lib/libcap.dylib"
        } else {
            ".lake/build/lib/libcap.so"
        });
        let mut cc = Command::new("cc");
        cc.arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .arg("-fPIC")
        .arg(&source)
        .arg("-o")
        .arg(&library);
        let output = cc.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let path = root.join(".lake/capability.json");
        let json=serde_json::to_string(&serde_json::json!({"schema_version":lean_toolchain::CAPABILITY_MANIFEST_SCHEMA_VERSION,"package":"lean_rs_host_shims","module":"LeanRsHostShims","primary_dylib":library,"dependencies":[],"exports":[]})).unwrap();
        fs::write(&path, &json).unwrap();
        let descriptor = LeanWorkerPreparedHostArtifacts {
            protocol: 1,
            shim_manifest_path: path.to_str().unwrap().to_owned(),
            shim_manifest_json: json,
            artifact_roots: vec![root.to_str().unwrap().to_owned()],
            shim_search_paths: vec![root.join(".lake/build/lib/lean").to_str().unwrap().to_owned(); 2],
        };
        Self {
            root,
            helper,
            registry,
            descriptor,
        }
    }
    fn worker(&self, guardian: bool) -> LeanWorker {
        let config = LeanWorkerConfig::new(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
            .shared_artifact_reads(true)
            .env("LEAN_HOST_MCP_WRITER_EXECUTABLE", &self.helper)
            .env("LEAN_RS_WORKER_OWNERS_DIRECTORY", &self.registry)
            .env(
                "LEAN_RS_PREPARED_HOST_ARTIFACTS",
                serde_json::to_string(&self.descriptor).unwrap(),
            )
            .env("LEAN_PRELOAD_MARKER", self.root.join("constructor-marker"))
            .request_timeout(Duration::from_secs(5));
        let config = if guardian {
            config.lifetime_directory(&self.registry).native_guardian(&self.helper)
        } else {
            config
        };
        LeanWorker::spawn(&config).expect("small guarded worker handshake")
    }
    fn session(&self) -> LeanWorkerSessionConfig {
        LeanWorkerSessionConfig::shims_only(&self.root, std::iter::empty::<String>())
    }
    fn marker(&self) -> PathBuf {
        self.root.join("constructor-marker")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _remove = fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "requires current H helper plus canonical private ownership registry; run serially"]
fn regression_exclusive_writer_blocks_constructor_until_admitted_and_acknowledged() {
    let fixture = Fixture::new();
    let writer = SharedWriter::open(&fixture.helper, &fixture.root, Duration::from_secs(2)).expect("exclusive writer");
    let mut worker = fixture.worker(true);
    let session = fixture.session();
    let (sender, receiver) = mpsc::channel();
    let child = std::thread::spawn(move || {
        let result = worker
            .open_session(&session, None, None)
            .map(|_| ())
            .map_err(|error| error.to_string());
        let shutdown = worker.try_shutdown().map(|_| ()).map_err(|error| error.to_string());
        sender.send((result, shutdown)).unwrap();
    });
    assert!(
        receiver.recv_timeout(Duration::from_millis(200)).is_err(),
        "request must wait for EX custody"
    );
    assert!(!fixture.marker().exists(), "native constructor ran before read custody");
    drop(writer);
    let (result, shutdown) = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("post-release worker result");
    assert!(
        fixture.marker().exists(),
        "fixture constructor must be reachable after release"
    );
    assert!(
        result
            .expect_err("deliberately null initializer")
            .contains("null IO result")
    );
    shutdown.expect("generation reaped with guardian");
    child.join().unwrap();
}

#[test]
#[ignore = "requires current H helper plus canonical private ownership registry; run serially"]
fn regression_absent_guardian_refuses_before_constructor() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker(false);
    let result = worker
        .open_session(&fixture.session(), None, None)
        .map(|_| ())
        .map_err(|error| error.to_string());
    assert!(
        result
            .expect_err("unguarded load must refuse")
            .contains("guardian custody")
    );
    assert!(!fixture.marker().exists());
    worker.try_shutdown().expect("pure worker reaped");
}

struct PausedGuardian(u32);
impl PausedGuardian {
    fn new(registry: &std::path::Path) -> Self {
        let entry = fs::read_dir(registry)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .expect("owned generation");
        let owner: serde_json::Value = serde_json::from_slice(&fs::read(entry.join("owner.json")).unwrap()).unwrap();
        let pid = u32::try_from(owner.get("guardian").unwrap().get("pid").unwrap().as_u64().unwrap()).unwrap();
        assert!(
            Command::new("kill")
                .args(["-STOP", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        // No Open has been dispatched: the worker owns the guardian's actual
        // unreaped Child. After STOP the live stopped process itself pins its
        // PID. Native deadline cleanup kills only the payload group, while the
        // guardian has its own group; closing its controls cannot wake STOP.
        // Thus panic/timeout cleanup below still resumes the same live guardian.
        let paused = Self(pid);
        let started = std::time::Instant::now();
        loop {
            let state = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&state.stdout).contains('T') {
                return paused;
            }
            assert!(started.elapsed() < Duration::from_secs(2), "guardian did not stop");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for PausedGuardian {
    fn drop(&mut self) {
        let _resume = Command::new("kill").args(["-CONT", &self.0.to_string()]).status();
    }
}

#[test]
#[ignore = "requires current H helper plus canonical private ownership registry; run serially"]
fn regression_authenticated_guardian_must_ack_before_constructor() {
    let fixture = Fixture::new();
    let mut worker = fixture.worker(true);
    // Bootstrap's authenticated durable-owner ACK is complete. Pause that exact
    // live guardian before the first FD loan; no retain ACK can be published.
    let pause = PausedGuardian::new(&fixture.registry);
    let session = fixture.session();
    let (sender, receiver) = mpsc::channel();
    let child = std::thread::spawn(move || {
        let result = worker
            .open_session(&session, None, None)
            .map(|_| ())
            .map_err(|error| error.to_string());
        let shutdown = worker.try_shutdown().map(|_| ()).map_err(|error| error.to_string());
        sender.send((result, shutdown)).unwrap();
    });
    let lock = lean_toolchain::shared_writer::project_lock_path(&fixture.root).unwrap();
    let started = std::time::Instant::now();
    loop {
        if let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&lock)
            && file.try_lock().is_err()
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "worker never acquired shared custody"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        receiver.recv_timeout(Duration::from_millis(200)).is_err(),
        "worker proceeded without retain ACK"
    );
    assert!(
        !fixture.marker().exists(),
        "constructor ran after SH but before durable guardian ACK"
    );
    drop(pause);
    let (result, shutdown) = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("resume permits ACK and terminal load result");
    assert!(
        fixture.marker().exists(),
        "constructor must run once genuine guardian ACK resumes"
    );
    assert!(
        result
            .expect_err("deliberately null initializer")
            .contains("null IO result")
    );
    shutdown.expect("owned generation reaped");
    child.join().unwrap();
}

#[test]
#[ignore = "requires current H helper plus canonical private ownership registry; run serially"]
fn regression_changed_manifest_cannot_load_dependency_outside_admitted_domains() {
    let fixture = Fixture::new();
    let dependency = fixture.root.join("unadmitted");
    fs::create_dir_all(dependency.join(".lake/build/lib")).unwrap();
    let manifest = std::path::Path::new(&fixture.descriptor.shim_manifest_path);
    let mut changed: serde_json::Value = serde_json::from_str(&fixture.descriptor.shim_manifest_json).unwrap();
    let original = changed.get("primary_dylib").unwrap().as_str().unwrap();
    let library = dependency.join(if cfg!(target_os = "macos") {
        ".lake/build/lib/libdep.dylib"
    } else {
        ".lake/build/lib/libdep.so"
    });
    fs::copy(original, &library).unwrap();
    changed
        .as_object_mut()
        .unwrap()
        .insert("dependencies".to_owned(), serde_json::json!([{"dylib_path":library}]));
    fs::write(manifest, serde_json::to_vec(&changed).unwrap()).unwrap();
    let writer = SharedWriter::open(&fixture.helper, &dependency, Duration::from_secs(2))
        .expect("exclusive unadmitted dependency");
    let mut worker = fixture.worker(true);
    let result = worker
        .open_session(&fixture.session(), None, None)
        .map(|_| ())
        .map_err(|error| error.to_string());
    assert!(
        result
            .expect_err("changed manifest must refuse")
            .contains("manifest changed")
    );
    assert!(
        !fixture.marker().exists(),
        "changed dependency was loaded without its admitted domain"
    );
    worker.try_shutdown().expect("refused generation closed");
    drop(writer);
}
