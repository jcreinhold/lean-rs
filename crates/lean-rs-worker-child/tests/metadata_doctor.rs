#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerCapabilityFact, LeanWorkerCommandMetadata, LeanWorkerConfig, LeanWorkerDoctorDiagnostic,
    LeanWorkerDoctorSeverity, LeanWorkerError, LeanWorkerSessionConfig,
};
use serde_json::json;

fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn interop_root() -> PathBuf {
    workspace_root().join("fixtures").join("interop-shims")
}

fn ensure_interop_built() {
    let fixture = interop_root();
    lean_toolchain::build_lake_target_quiet(&fixture, "LeanRsInteropConsumer")
        .expect("interop consumer Lake target builds");
}

fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary())
}

fn session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        interop_root(),
        "lean_rs_interop_consumer",
        "LeanRsInteropConsumer",
        ["LeanRsInteropConsumer.Callback"],
    )
}

#[test]
fn runtime_and_capability_metadata_are_separate() {
    ensure_interop_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let runtime = worker.runtime_metadata();
    assert_eq!(runtime.worker_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(runtime.protocol_version, 3);
    assert_eq!(runtime.lean_version, None);

    let mut session = worker
        .open_session(&session_config(), None, None)
        .expect("worker session opens");
    let metadata = session
        .capability_metadata(
            "lean_rs_interop_consumer_worker_metadata",
            &json!({"caller": "metadata-test"}),
            None,
            None,
        )
        .expect("metadata export succeeds");

    assert_eq!(
        metadata.commands,
        vec![
            LeanWorkerCommandMetadata {
                name: "version".to_owned(),
                version: "fixture-1".to_owned(),
            },
            LeanWorkerCommandMetadata {
                name: "scan".to_owned(),
                version: "fixture-2".to_owned(),
            },
        ],
    );
    assert_eq!(
        metadata.capabilities,
        vec![
            LeanWorkerCapabilityFact {
                name: "rows.json".to_owned(),
                version: "fixture-1".to_owned(),
            },
            LeanWorkerCapabilityFact {
                name: "diagnostics".to_owned(),
                version: "fixture-1".to_owned(),
            },
        ],
    );
    assert_eq!(metadata.lean_version, Some("fixture-lean-4".to_owned()));
    assert_eq!(metadata.extra, Some(json!({"fixture": true})));
}

#[test]
fn doctor_reports_pass_warning_and_error_diagnostics() {
    ensure_interop_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&session_config(), None, None)
        .expect("worker session opens");

    let report = session
        .capability_doctor(
            "lean_rs_interop_consumer_worker_doctor",
            &json!({"check": "all"}),
            None,
            None,
        )
        .expect("doctor export succeeds");

    assert_eq!(
        report.diagnostics,
        vec![
            LeanWorkerDoctorDiagnostic {
                severity: LeanWorkerDoctorSeverity::Pass,
                code: "fixture.ok".to_owned(),
                message: "fixture ready".to_owned(),
                details: Some(json!({"check": "load"})),
            },
            LeanWorkerDoctorDiagnostic {
                severity: LeanWorkerDoctorSeverity::Warning,
                code: "fixture.warning".to_owned(),
                message: "optional fixture warning".to_owned(),
                details: Some(json!({"optional": true})),
            },
            LeanWorkerDoctorDiagnostic {
                severity: LeanWorkerDoctorSeverity::Error,
                code: "fixture.error".to_owned(),
                message: "fixture error example".to_owned(),
                details: Some(json!({"recoverable": false})),
            },
        ],
    );
    assert_eq!(report.metadata, Some(json!({"fixture": "doctor"})));
}

#[test]
fn malformed_metadata_and_doctor_json_are_typed() {
    ensure_interop_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .capability_metadata(
            "lean_rs_interop_consumer_worker_metadata_malformed",
            &json!({}),
            None,
            None,
        )
        .expect_err("malformed metadata should be typed");
    match err {
        LeanWorkerError::CapabilityMetadataMalformed { message } => {
            assert!(
                message.contains("key must be a string") || message.contains("expected ident"),
                "unexpected message: {message}",
            );
        }
        other => panic!("expected metadata malformed error, got {other:?}"),
    }

    let err = session
        .capability_doctor(
            "lean_rs_interop_consumer_worker_doctor_malformed",
            &json!({}),
            None,
            None,
        )
        .expect_err("malformed doctor should be typed");
    match err {
        LeanWorkerError::CapabilityDoctorMalformed { message } => {
            assert!(message.contains("unknown variant"), "unexpected message: {message}");
        }
        other => panic!("expected doctor malformed error, got {other:?}"),
    }
}

#[test]
fn missing_metadata_and_doctor_exports_are_typed_worker_errors() {
    ensure_interop_built();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");
    let mut session = worker
        .open_session(&session_config(), None, None)
        .expect("worker session opens");

    let err = session
        .capability_metadata(
            "lean_rs_interop_consumer_worker_metadata_missing",
            &json!({}),
            None,
            None,
        )
        .expect_err("missing metadata export should fail");
    match err {
        LeanWorkerError::Worker { code, message } => {
            assert_eq!(code, "lean_rs.symbol_lookup");
            assert!(
                message.contains("lean_rs_interop_consumer_worker_metadata_missing"),
                "message should name missing export, got {message}",
            );
        }
        other => panic!("expected worker symbol lookup error, got {other:?}"),
    }

    let err = session
        .capability_doctor("lean_rs_interop_consumer_worker_doctor_missing", &json!({}), None, None)
        .expect_err("missing doctor export should fail");
    match err {
        LeanWorkerError::Worker { code, message } => {
            assert_eq!(code, "lean_rs.symbol_lookup");
            assert!(
                message.contains("lean_rs_interop_consumer_worker_doctor_missing"),
                "message should name missing export, got {message}",
            );
        }
        other => panic!("expected worker symbol lookup error, got {other:?}"),
    }
}
