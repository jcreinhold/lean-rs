#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Small native constructor fixtures; these never build or import Mathlib.
//! Opt in with `LEAN_TOOLCHAIN_READER_TEST_HELPER` and a canonical, private
//! `LEAN_RS_WORKER_OWNERS_DIRECTORY` (0700), then run this suite serially.
use std::fs;
use std::os::unix::fs::MetadataExt as _;
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
#include <fcntl.h>
#include <unistd.h>
__attribute__((constructor)) static void mark_load(void) {
  const char *report=getenv("LEAN_PRELOAD_CUSTODY_REPORT");
  if(report) {
    /* Capture the FIRST constructor's observation; later libraries cannot
       overwrite evidence of premature loading with a later complete record. */
    int fd=open(report,O_WRONLY|O_CREAT|O_EXCL,0600);
    if(fd>=0) {
      FILE *output=fdopen(fd,"w");
      if(output) {
        const char *registry=getenv("LEAN_RS_WORKER_OWNERS_DIRECTORY");
        const char *nonce=getenv("LEAN_RS_WORKER_PARENT_NONCE");
        char record[8192];
        int length=snprintf(record,sizeof(record),"%s/worker-%s/owner.json",registry?registry:"",nonce?nonce:"");
        fprintf(output,"{\"constructor_pid\":%ld,\"nonce\":\"%s\",\"owner\":",(long)getpid(),nonce?nonce:"");
        FILE *input=(length>0 && (size_t)length<sizeof(record))?fopen(record,"r"):NULL;
        if(input) {
          char bytes[4096];size_t count,total=0;
          while(total<262144 && (count=fread(bytes,1,sizeof(bytes)<262144-total?sizeof(bytes):262144-total,input))>0) {
            fwrite(bytes,1,count,output);total+=count;
          }
          fclose(input);
        } else { fputs("null",output); }
        fputs("}\n",output);fclose(output);
      } else { close(fd); }
    }
  }
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
            .env(
                "LEAN_PRELOAD_CUSTODY_REPORT",
                self.root.join("constructor-custody.json"),
            )
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

#[test]
#[ignore = "requires current H helper plus canonical private ownership registry; run serially"]
fn regression_first_constructor_observes_durable_complete_guardian_custody() {
    let mut fixture = Fixture::new();
    let dependency = fixture.root.join("dependency");
    fs::create_dir_all(dependency.join(".lake/build/lib")).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&fixture.descriptor.shim_manifest_json).unwrap();
    let primary = manifest.get("primary_dylib").unwrap().as_str().unwrap();
    let library = dependency.join(if cfg!(target_os = "macos") {
        ".lake/build/lib/libdep.dylib"
    } else {
        ".lake/build/lib/libdep.so"
    });
    fs::copy(primary, &library).unwrap();
    manifest
        .as_object_mut()
        .unwrap()
        .insert("dependencies".to_owned(), serde_json::json!([{"dylib_path": library}]));
    fixture.descriptor.shim_manifest_json = serde_json::to_string(&manifest).unwrap();
    fs::write(
        &fixture.descriptor.shim_manifest_path,
        &fixture.descriptor.shim_manifest_json,
    )
    .unwrap();
    fixture
        .descriptor
        .artifact_roots
        .push(dependency.to_str().unwrap().to_owned());
    fixture.descriptor.artifact_roots.sort();

    let mut worker = fixture.worker(true);
    let result = worker
        .open_session(&fixture.session(), None, None)
        .map(|_| ())
        .map_err(|error| error.to_string());
    assert!(
        result
            .expect_err("deliberately null initializer")
            .contains("null IO result")
    );
    assert!(fixture.marker().exists(), "the native constructor must actually run");
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join("constructor-custody.json")).unwrap()).unwrap();
    let owner = report.get("owner").expect("constructor's exact persisted owner record");
    assert_eq!(owner.get("version").and_then(serde_json::Value::as_u64), Some(2));
    assert_eq!(owner.get("phase").and_then(serde_json::Value::as_str), Some("running"));
    assert_eq!(owner.get("generation_id"), report.get("nonce"));
    let pid = report
        .get("constructor_pid")
        .and_then(serde_json::Value::as_u64)
        .unwrap();
    assert!(pid > 1);
    let process = owner.get("process").unwrap();
    for field in ["pid", "process_group", "session"] {
        assert_eq!(process.get(field).and_then(serde_json::Value::as_u64), Some(pid));
    }
    let guardian = owner.get("guardian").unwrap();
    assert_eq!(guardian.get("session").and_then(serde_json::Value::as_u64), Some(pid));
    assert_ne!(guardian.get("pid").and_then(serde_json::Value::as_u64), Some(pid));

    let observed: Vec<lean_toolchain::native_guardian::NativeReadDomain> =
        serde_json::from_value(owner.get("protected_domains").unwrap().clone()).unwrap();
    let mut expected_roots = vec![fixture.root.clone(), dependency];
    expected_roots.sort();
    let mut observed_roots: Vec<_> = observed.iter().map(|domain| domain.project_root.clone()).collect();
    observed_roots.sort();
    assert_eq!(
        observed_roots, expected_roots,
        "the FIRST loaded library requires the full union"
    );
    let mut lock_files = Vec::new();
    for root in expected_roots {
        let domain = observed.iter().find(|domain| domain.project_root == root).unwrap();
        let lock = lean_toolchain::shared_writer::project_lock_path(&root).unwrap();
        assert_eq!(domain.lock_path, lock);
        let file = fs::OpenOptions::new().read(true).write(true).open(&lock).unwrap();
        let metadata = file.metadata().unwrap();
        assert_eq!((domain.device, domain.inode), (metadata.dev(), metadata.ino()));
        assert!(!domain.epoch.is_empty());
        lock_files.push(file);
    }
    // The known initializer refusal dropped the worker's request aliases.
    // Explicit logical release still cannot release the guardian's generation FDs.
    worker.release_host_read().expect("logical read release");
    for file in &lock_files {
        assert!(
            matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock)),
            "guardian must retain the actual shared descriptions"
        );
    }
    worker.try_shutdown().expect("owned generation reaped");
    for file in lock_files {
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) => {
                    assert!(
                        started.elapsed() < Duration::from_secs(2),
                        "guardian custody did not close"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("exclusive lock probe failed: {error}"),
            }
        }
    }
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
