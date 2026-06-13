#![allow(
    clippy::exit,
    clippy::expect_used,
    clippy::panic,
    clippy::unnecessary_wraps,
    clippy::wildcard_enum_match_arm
)]

use std::env;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[cfg(all(unix, not(target_os = "linux")))]
use std::process::Command;

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerCancellationToken, LeanWorkerCapabilityBuilder, LeanWorkerChild, LeanWorkerConfig,
    LeanWorkerDataRow, LeanWorkerDataSink, LeanWorkerDeclarationVerificationBatchItem,
    LeanWorkerDeclarationVerificationBatchRequest, LeanWorkerDeclarationVerificationTarget, LeanWorkerError,
    LeanWorkerPool, LeanWorkerPoolConfig, LeanWorkerRestartPolicy, LeanWorkerShutdownOutcome, LeanWorkerSorryPolicy,
};
use lean_rs_worker_protocol::protocol::{
    DataRowEmitter, MAX_FRAME_BYTES, Message, PROTOCOL_VERSION, Request, Response, read_frame, write_frame,
};
use lean_rs_worker_protocol::types::{
    LeanWorkerDeclarationOutlineResult, LeanWorkerDeclarationTargetInfo, LeanWorkerDeclarationVerificationBatchResult,
    LeanWorkerDeclarationVerificationBatchRow, LeanWorkerDeclarationVerificationFacts,
    LeanWorkerDeclarationVerificationStatus, LeanWorkerDiagnostic, LeanWorkerElabFailure, LeanWorkerElabOptions,
    LeanWorkerImportStats, LeanWorkerModuleCacheStatus, LeanWorkerModuleQueryBatchEnvelope,
    LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryBatchResult,
    LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQuerySelector, LeanWorkerModuleQueryTimings,
    LeanWorkerModuleSourceSpan, LeanWorkerOutputBudgets, LeanWorkerSessionImportProfile,
};
use lean_toolchain::LeanBuiltCapability;
use serde_json::value::RawValue;

const FAKE_CHILD_ENV: &str = "LEAN_RS_WORKER_PARENT_FAKE_CHILD";

// Import-light conformance harness for the worker-parent runtime model.
//
// The test binary re-enters itself as a fake worker child through `FAKE_CHILD_ENV`, speaks
// deterministic worker-protocol frames, and never opens a Lean session or fixture capability.
// `RuntimeTraceEvent` below names the model-level observations these tests protect. Real
// Lean integration still belongs in the worker-child tests. Exact command:
//
//   cargo nextest run -p lean-rs-worker-parent --profile ci
//
fn main() {
    if let Ok(mode) = env::var(FAKE_CHILD_ENV) {
        run_fake_child(&mode);
        return;
    }

    let tests: &[(&str, fn() -> Result<(), String>)] = &[
        (
            "conformance_terminal_success_has_one_terminal_outcome",
            conformance_terminal_success_has_one_terminal_outcome,
        ),
        (
            "conformance_stream_rows_are_tentative_until_terminal_success",
            conformance_stream_rows_are_tentative_until_terminal_success,
        ),
        (
            "conformance_stream_child_exit_after_rows_discards_tentative_rows",
            conformance_stream_child_exit_after_rows_discards_tentative_rows,
        ),
        (
            "conformance_stream_child_crash_after_rows_discards_tentative_rows",
            conformance_stream_child_crash_after_rows_discards_tentative_rows,
        ),
        (
            "conformance_stream_timeout_after_rows_discards_tentative_rows",
            conformance_stream_timeout_after_rows_discards_tentative_rows,
        ),
        (
            "conformance_stream_cancellation_after_rows_discards_tentative_rows",
            conformance_stream_cancellation_after_rows_discards_tentative_rows,
        ),
        (
            "conformance_stream_backpressure_is_bounded_and_observable",
            conformance_stream_backpressure_is_bounded_and_observable,
        ),
        (
            "conformance_explicit_shutdown_gracefully_reaps_child",
            conformance_explicit_shutdown_gracefully_reaps_child,
        ),
        (
            "conformance_dropped_idle_worker_reaps_child",
            conformance_dropped_idle_worker_reaps_child,
        ),
        (
            "conformance_dropped_worker_escalates_kill_and_reaps_child",
            conformance_dropped_worker_escalates_kill_and_reaps_child,
        ),
        (
            "conformance_timeout_kill_reap_restarts_next_generation",
            conformance_timeout_kill_reap_restarts_next_generation,
        ),
        (
            "conformance_child_crash_terminalizes_in_flight_request",
            conformance_child_crash_terminalizes_in_flight_request,
        ),
        (
            "conformance_restart_limit_exhaustion_is_typed_terminal_outcome",
            conformance_restart_limit_exhaustion_is_typed_terminal_outcome,
        ),
        (
            "conformance_pool_lease_drop_releases_capacity_once",
            conformance_pool_lease_drop_releases_capacity_once,
        ),
        (
            "conformance_pool_explicit_release_decrements_capacity_once",
            conformance_pool_explicit_release_decrements_capacity_once,
        ),
        (
            "conformance_pool_idle_replacement_preserves_capacity_accounting",
            conformance_pool_idle_replacement_preserves_capacity_accounting,
        ),
        (
            "conformance_pool_admission_refusal_is_explicit",
            conformance_pool_admission_refusal_is_explicit,
        ),
        (
            "declaration_outline_batch_selector_reaches_parent_session_path",
            declaration_outline_batch_selector_reaches_parent_session_path,
        ),
        (
            "declaration_verification_batch_reaches_parent_session_path",
            declaration_verification_batch_reaches_parent_session_path,
        ),
        (
            "command_message_diagnostics_selector_reaches_parent_session_path",
            command_message_diagnostics_selector_reaches_parent_session_path,
        ),
    ];

    for (name, test) in tests {
        test().unwrap_or_else(|err| panic!("{name}: {err}"));
    }
}

fn run_fake_child(mode: &str) {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    write_frame(
        &mut stdout,
        Message::Handshake {
            worker_version: "fake-worker-shutdown-test".to_owned(),
            protocol_version: PROTOCOL_VERSION,
        },
        MAX_FRAME_BYTES,
    )
    .expect("fake child writes handshake");

    let frame_limit = {
        let mut stdin = io::stdin().lock();
        read_frame(&mut stdin, MAX_FRAME_BYTES).expect("fake child reads frame-limit")
    };
    match frame_limit.message {
        Message::ConfigureFrameLimit { .. } => {}
        other => panic!("expected frame-limit configuration, got {other:?}"),
    }

    loop {
        let frame = {
            let mut stdin = io::stdin().lock();
            let Ok(frame) = read_frame(&mut stdin, MAX_FRAME_BYTES) else {
                return;
            };
            frame
        };
        let Message::Request(request) = frame.message else {
            continue;
        };
        match request {
            Request::Health if mode == "request_hang" => sleep_forever(),
            Request::Health if mode == "crash_on_health" => std::process::exit(42),
            Request::Health => {
                write_frame(&mut stdout, Message::Response(Response::HealthOk), MAX_FRAME_BYTES)
                    .expect("fake child writes health response");
            }
            Request::EmitTestRows { streams } if mode == "rows_then_hang" => {
                let mut emitter = DataRowEmitter::default();
                for stream in streams {
                    let payload = RawValue::from_string(format!(r#"{{"stream":"{stream}"}}"#))
                        .expect("fake child builds JSON row payload");
                    let row = emitter.next(stream, payload);
                    write_frame(&mut stdout, Message::DataRow(row), MAX_FRAME_BYTES)
                        .expect("fake child writes data row");
                }
                sleep_forever();
            }
            Request::EmitTestRows { streams } => {
                let mut emitter = DataRowEmitter::default();
                for stream in streams {
                    let payload = RawValue::from_string(format!(r#"{{"stream":"{stream}"}}"#))
                        .expect("fake child builds JSON row payload");
                    let row = emitter.next(stream, payload);
                    write_frame(&mut stdout, Message::DataRow(row), MAX_FRAME_BYTES)
                        .expect("fake child writes data row");
                }
                write_frame(
                    &mut stdout,
                    Message::Response(Response::RowsComplete { count: emitter.count() }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake child writes row terminal response");
            }
            Request::EmitTestRowsThenExit => {
                let mut emitter = DataRowEmitter::default();
                let payload = RawValue::from_string(r#"{"stream":"rows"}"#.to_owned())
                    .expect("fake child builds JSON row payload");
                let row = emitter.next("rows", payload);
                write_frame(&mut stdout, Message::DataRow(row), MAX_FRAME_BYTES).expect("fake child writes data row");
                return;
            }
            Request::EmitTestRowsThenPanic => {
                let mut emitter = DataRowEmitter::default();
                let payload = RawValue::from_string(r#"{"stream":"rows"}"#.to_owned())
                    .expect("fake child builds JSON row payload");
                let row = emitter.next("rows", payload);
                write_frame(&mut stdout, Message::DataRow(row), MAX_FRAME_BYTES).expect("fake child writes data row");
                std::process::abort();
            }
            Request::OpenHostSession {
                imports,
                import_profile,
                ..
            } => {
                write_frame(
                    &mut stdout,
                    Message::Response(Response::HostSessionOpened {
                        import_stats: fake_import_stats(imports, import_profile),
                    }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake child writes session-open response");
            }
            Request::ProcessModuleQueryBatch { selectors, .. } => {
                let items = selectors
                    .into_iter()
                    .map(|selector| match selector {
                        LeanWorkerModuleQuerySelector::Diagnostics { id } => LeanWorkerModuleQueryBatchItem::Ok {
                            id,
                            result: Box::new(LeanWorkerModuleQueryBatchResult::Diagnostics(
                                fake_command_message_diagnostics(),
                            )),
                        },
                        LeanWorkerModuleQuerySelector::DeclarationOutline { id } => {
                            LeanWorkerModuleQueryBatchItem::Ok {
                                id,
                                result: Box::new(LeanWorkerModuleQueryBatchResult::DeclarationOutline(
                                    fake_declaration_outline(),
                                )),
                            }
                        }
                        other => LeanWorkerModuleQueryBatchItem::Unavailable {
                            id: other.id().to_owned(),
                            message: "fake child only implements diagnostics and declaration outline".to_owned(),
                        },
                    })
                    .collect();
                write_frame(
                    &mut stdout,
                    Message::Response(Response::ProcessModuleQueryBatch {
                        outcome: LeanWorkerModuleQueryBatchOutcome::Ok {
                            result: LeanWorkerModuleQueryBatchEnvelope {
                                items,
                                total_truncated: false,
                            },
                            imports: Vec::new(),
                            facts: LeanWorkerModuleQueryCacheFacts {
                                cache_status: LeanWorkerModuleCacheStatus::Miss,
                                timings: LeanWorkerModuleQueryTimings::zero(),
                                output_bytes: 0,
                                cache_entry_count: None,
                                cache_approx_bytes: None,
                                resource: None,
                            },
                        },
                    }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake child writes module query batch response");
            }
            Request::VerifyDeclarationBatch { request, .. } => {
                let rows = request
                    .targets
                    .into_iter()
                    .map(|target| LeanWorkerDeclarationVerificationBatchRow {
                        id: target.id,
                        target: target.target,
                        verification_status: LeanWorkerDeclarationVerificationStatus::Accepted,
                        facts: Box::new(LeanWorkerDeclarationVerificationFacts::unavailable()),
                    })
                    .collect();
                write_frame(
                    &mut stdout,
                    Message::Response(Response::DeclarationVerificationBatch {
                        result: LeanWorkerDeclarationVerificationBatchResult::Ok {
                            results: rows,
                            imports: Vec::new(),
                        },
                    }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake child writes declaration verification batch response");
            }
            Request::Terminate if mode == "terminate_hang" => sleep_forever(),
            Request::Terminate => {
                write_frame(&mut stdout, Message::Response(Response::Terminating), MAX_FRAME_BYTES)
                    .expect("fake child writes terminating response");
                stdout.flush().expect("fake child flushes terminating response");
                return;
            }
            other => {
                write_frame(
                    &mut stdout,
                    Message::Response(Response::Error {
                        code: "fake.unsupported".to_owned(),
                        message: format!("unsupported fake request: {other:?}"),
                    }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake child writes unsupported response");
            }
        }
    }
}

fn fake_import_stats(
    direct_import_names: Vec<String>,
    import_profile: LeanWorkerSessionImportProfile,
) -> LeanWorkerImportStats {
    LeanWorkerImportStats {
        direct_import_names,
        effective_module_count: 1,
        compacted_region_count: 0,
        memory_mapped_region_count: 0,
        compacted_region_bytes: 0,
        memory_mapped_region_bytes: 0,
        non_memory_mapped_region_bytes: 0,
        imported_bytes: 0,
        imported_constant_count: 0,
        extension_count: 0,
        total_imported_extension_entries: 0,
        import_level: import_profile.label().to_owned(),
        import_all: false,
        load_exts: false,
    }
}

fn fake_declaration_outline() -> LeanWorkerDeclarationOutlineResult {
    let span = LeanWorkerModuleSourceSpan {
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 20,
    };
    LeanWorkerDeclarationOutlineResult {
        declarations: vec![LeanWorkerDeclarationTargetInfo {
            short_name: "outlined".to_owned(),
            declaration_name: "Fake.outlined".to_owned(),
            namespace_name: "Fake".to_owned(),
            declaration_kind: "theorem".to_owned(),
            declaration_span: span.clone(),
            name_span: span.clone(),
            body_span: span,
        }],
        truncated: false,
    }
}

fn fake_command_message_diagnostics() -> LeanWorkerElabFailure {
    LeanWorkerElabFailure {
        diagnostics: vec![LeanWorkerDiagnostic {
            severity: "info".to_owned(),
            message: "Nat.add : Nat -> Nat -> Nat".to_owned(),
            file_label: "/fake-command-message.lean".to_owned(),
            line: Some(1),
            column: Some(1),
            end_line: None,
            end_column: None,
        }],
        truncated: false,
    }
}

fn sleep_forever() -> ! {
    loop {
        thread::sleep(Duration::from_mins(1));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeTraceEvent {
    GenerationStarted(u64),
    RequestAdmitted {
        generation: u64,
        request: &'static str,
    },
    RequestSent {
        generation: u64,
        request: &'static str,
    },
    StreamRowObserved {
        generation: u64,
        request: &'static str,
        stream: String,
        sequence: u64,
    },
    TerminalOutcomeObserved {
        generation: u64,
        request: &'static str,
        outcome: RuntimeTerminalOutcome,
    },
    BackpressureObserved {
        generation: u64,
        request: &'static str,
        waits: u64,
    },
    TimeoutObserved {
        generation: u64,
        request: &'static str,
    },
    ChildCrashObserved {
        generation: u64,
        request: &'static str,
    },
    RestartObserved {
        from: u64,
        to: u64,
    },
    RestartLimitExhausted {
        generation: u64,
    },
    ShutdownStarted {
        generation: u64,
    },
    GracefulStopAttempted {
        generation: u64,
    },
    KillEscalated {
        generation: u64,
    },
    ChildReaped {
        generation: u64,
    },
    LeaseGranted {
        active_workers: usize,
        warm_leases: usize,
    },
    LeaseDropped {
        active_workers: usize,
        warm_leases: usize,
    },
    LeaseReleased {
        active_workers: usize,
        warm_leases: usize,
    },
    IdleReplacementObserved {
        policy_restarts: u64,
        worker_restarts: u64,
    },
    AdmissionRefused {
        reason: &'static str,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeTerminalOutcome {
    Response(&'static str),
    Timeout,
    Cancelled,
    ChildExited,
    ChildPanicOrAbort,
    RestartLimitExceeded,
    ShutdownInProgress,
}

fn request_trace(generation: u64, request: &'static str) -> Vec<RuntimeTraceEvent> {
    vec![
        RuntimeTraceEvent::GenerationStarted(generation),
        RuntimeTraceEvent::RequestAdmitted { generation, request },
        RuntimeTraceEvent::RequestSent { generation, request },
    ]
}

fn assert_single_terminal(trace: &[RuntimeTraceEvent], generation: u64, request: &'static str) -> Result<(), String> {
    let terminal_count = trace
        .iter()
        .filter(|event| {
            matches!(
                event,
                RuntimeTraceEvent::TerminalOutcomeObserved {
                    generation: observed_generation,
                    request: observed_request,
                    ..
                } if *observed_generation == generation && *observed_request == request
            )
        })
        .count();
    if terminal_count == 1 {
        Ok(())
    } else {
        Err(format!(
            "expected exactly one terminal outcome for generation {generation} request {request}, got {terminal_count}: {trace:?}"
        ))
    }
}

fn fake_worker(mode: &str) -> Result<LeanWorker, LeanWorkerError> {
    fake_worker_with_config(mode, |config| config)
}

fn fake_worker_with_config(
    mode: &str,
    configure: impl FnOnce(LeanWorkerConfig) -> LeanWorkerConfig,
) -> Result<LeanWorker, LeanWorkerError> {
    let executable = env::current_exe().map_err(|source| LeanWorkerError::Spawn {
        executable: "<current test executable>".into(),
        source,
    })?;
    let config = LeanWorkerConfig::new(executable)
        .env(FAKE_CHILD_ENV, mode)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_millis(80))
        .shutdown_timeout(Duration::from_millis(80));
    LeanWorker::spawn(&configure(config))
}

fn conformance_terminal_success_has_one_terminal_outcome() -> Result<(), String> {
    let mut worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let before = worker.lifecycle_snapshot();
    let before_stats = worker.stats();
    assert_eq!(before.worker_generation, 0);
    assert_eq!(before_stats.requests, 0);
    let mut trace = request_trace(before.worker_generation, "health");

    worker.health().map_err(|err| err.to_string())?;
    trace.push(RuntimeTraceEvent::TerminalOutcomeObserved {
        generation: before.worker_generation,
        request: "health",
        outcome: RuntimeTerminalOutcome::Response("health_ok"),
    });

    let after = worker.lifecycle_snapshot();
    assert_eq!(
        after.worker_generation, before.worker_generation,
        "terminal success must not replace the child generation"
    );
    assert_eq!(worker.stats().requests, 1, "one health request should be counted");
    assert_eq!(worker.stats().restarts, 0, "success must not restart the child");
    assert_single_terminal(&trace, before.worker_generation, "health")?;
    let report = worker.shutdown().map_err(|err| err.to_string())?;
    assert_eq!(report.outcome, LeanWorkerShutdownOutcome::Graceful);
    Ok(())
}

fn conformance_stream_rows_are_tentative_until_terminal_success() -> Result<(), String> {
    let mut worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let sink = RecordingSink::default();
    let mut trace = request_trace(generation, "emit_test_rows");
    let count = worker
        .__emit_test_rows(
            vec!["left".to_owned(), "right".to_owned(), "left".to_owned()],
            None,
            Some(&sink),
        )
        .map_err(|err| err.to_string())?;
    assert_eq!(count, 3);

    let rows = sink.rows();
    let observed: Vec<(String, u64)> = rows.into_iter().map(|row| (row.stream, row.sequence)).collect();
    assert_eq!(
        observed,
        vec![("left".to_owned(), 0), ("right".to_owned(), 0), ("left".to_owned(), 1),]
    );
    for (stream, sequence) in observed {
        trace.push(RuntimeTraceEvent::StreamRowObserved {
            generation,
            request: "emit_test_rows",
            stream,
            sequence,
        });
    }
    trace.push(RuntimeTraceEvent::TerminalOutcomeObserved {
        generation,
        request: "emit_test_rows",
        outcome: RuntimeTerminalOutcome::Response("rows_complete"),
    });
    assert_eq!(worker.stats().requests, 1);
    assert_eq!(worker.stats().data_rows_delivered, 3);
    assert_single_terminal(&trace, generation, "emit_test_rows")?;
    drop(worker);
    Ok(())
}

fn conformance_stream_child_exit_after_rows_discards_tentative_rows() -> Result<(), String> {
    let mut worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let sink = RecordingSink::default();
    let mut trace = request_trace(generation, "emit_test_rows_then_exit");
    let err = worker
        .__emit_test_rows_then_exit(None, Some(&sink))
        .expect_err("fake child should exit before terminal stream success");
    match err {
        LeanWorkerError::ChildExited { exit } => assert!(exit.success),
        other => return Err(format!("expected child-exited stream failure, got {other:?}")),
    }
    let rows = sink.rows();
    assert_eq!(rows.len(), 1);
    let first_row = rows.first().ok_or_else(|| "missing first stream row".to_owned())?;
    trace.push(RuntimeTraceEvent::StreamRowObserved {
        generation,
        request: "emit_test_rows_then_exit",
        stream: first_row.stream.clone(),
        sequence: first_row.sequence,
    });
    trace.push(RuntimeTraceEvent::TerminalOutcomeObserved {
        generation,
        request: "emit_test_rows_then_exit",
        outcome: RuntimeTerminalOutcome::ChildExited,
    });
    let stats = worker.stats();
    assert_eq!(stats.data_rows_delivered, 1);
    assert_eq!(stats.stream_failures, 1);
    assert_single_terminal(&trace, generation, "emit_test_rows_then_exit")?;
    drop(worker);
    Ok(())
}

fn conformance_stream_child_crash_after_rows_discards_tentative_rows() -> Result<(), String> {
    let mut worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let sink = RecordingSink::default();
    let mut trace = request_trace(generation, "emit_test_rows_then_panic");
    let err = worker
        .__emit_test_rows_then_panic(None, Some(&sink))
        .expect_err("fake child should abort before terminal stream success");
    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => assert!(!exit.success),
        other => return Err(format!("expected child-panic stream failure, got {other:?}")),
    }
    let rows = sink.rows();
    assert_eq!(rows.len(), 1);
    let first_row = rows.first().ok_or_else(|| "missing first stream row".to_owned())?;
    trace.push(RuntimeTraceEvent::StreamRowObserved {
        generation,
        request: "emit_test_rows_then_panic",
        stream: first_row.stream.clone(),
        sequence: first_row.sequence,
    });
    trace.push(RuntimeTraceEvent::TerminalOutcomeObserved {
        generation,
        request: "emit_test_rows_then_panic",
        outcome: RuntimeTerminalOutcome::ChildPanicOrAbort,
    });
    let stats = worker.stats();
    assert_eq!(stats.data_rows_delivered, 1);
    assert_eq!(stats.stream_failures, 1);
    assert_single_terminal(&trace, generation, "emit_test_rows_then_panic")?;
    drop(worker);
    Ok(())
}

fn conformance_stream_timeout_after_rows_discards_tentative_rows() -> Result<(), String> {
    let mut worker = fake_worker("rows_then_hang").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let old_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose an initial child pid".to_owned())?;
    let sink = RecordingSink::default();
    let mut trace = request_trace(generation, "emit_test_rows");
    let err = worker
        .__emit_test_rows(vec!["rows".to_owned()], None, Some(&sink))
        .expect_err("fake child should hang after tentative rows");
    match err {
        LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "emit_test_rows"),
        other => return Err(format!("expected timeout after stream rows, got {other:?}")),
    }
    let rows = sink.rows();
    assert_eq!(rows.len(), 1);
    let first_row = rows.first().ok_or_else(|| "missing first stream row".to_owned())?;
    trace.push(RuntimeTraceEvent::StreamRowObserved {
        generation,
        request: "emit_test_rows",
        stream: first_row.stream.clone(),
        sequence: first_row.sequence,
    });
    trace.push(RuntimeTraceEvent::TimeoutObserved {
        generation,
        request: "emit_test_rows",
    });
    trace.push(RuntimeTraceEvent::TerminalOutcomeObserved {
        generation,
        request: "emit_test_rows",
        outcome: RuntimeTerminalOutcome::Timeout,
    });
    assert_reaped(old_pid)?;
    let stats = worker.stats();
    assert_eq!(stats.data_rows_delivered, 1);
    assert_eq!(stats.stream_failures, 1);
    assert_eq!(stats.timeout_restarts, 1);
    assert_single_terminal(&trace, generation, "emit_test_rows")?;
    drop(worker);
    Ok(())
}

fn conformance_stream_cancellation_after_rows_discards_tentative_rows() -> Result<(), String> {
    let mut worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let old_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose an initial child pid".to_owned())?;
    let cancellation = LeanWorkerCancellationToken::new();
    let sink = CancellingSink::new(cancellation.clone());
    let mut trace = request_trace(generation, "emit_test_rows");
    let err = worker
        .__emit_test_rows(
            vec!["rows".to_owned(), "late".to_owned()],
            Some(&cancellation),
            Some(&sink),
        )
        .expect_err("sink cancellation should interrupt stream after the first row");
    match err {
        LeanWorkerError::Cancelled { operation, .. } => assert_eq!(operation, "emit_test_rows"),
        other => return Err(format!("expected cancellation after stream row, got {other:?}")),
    }
    let rows = sink.rows();
    assert_eq!(rows.len(), 1);
    let first_row = rows.first().ok_or_else(|| "missing first stream row".to_owned())?;
    trace.push(RuntimeTraceEvent::StreamRowObserved {
        generation,
        request: "emit_test_rows",
        stream: first_row.stream.clone(),
        sequence: first_row.sequence,
    });
    trace.push(RuntimeTraceEvent::TerminalOutcomeObserved {
        generation,
        request: "emit_test_rows",
        outcome: RuntimeTerminalOutcome::Cancelled,
    });
    assert_reaped(old_pid)?;
    let stats = worker.stats();
    assert_eq!(stats.data_rows_delivered, 1);
    assert_eq!(stats.stream_failures, 1);
    assert_eq!(stats.cancelled_restarts, 1);
    assert_single_terminal(&trace, generation, "emit_test_rows")?;
    drop(worker);
    Ok(())
}

fn conformance_stream_backpressure_is_bounded_and_observable() -> Result<(), String> {
    let mut worker = fake_worker_with_config("normal", |config| config.request_timeout(Duration::from_secs(15)))
        .map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let sink = SlowSink::new(Duration::from_millis(1));
    let streams = (0..5_000).map(|_| "rows".to_owned()).collect::<Vec<_>>();
    let count = worker
        .__emit_test_rows(streams, None, Some(&sink))
        .map_err(|err| err.to_string())?;
    assert_eq!(count, 5_000);
    assert_eq!(sink.rows().len(), 5_000);
    let stats = worker.stats();
    let trace = [RuntimeTraceEvent::BackpressureObserved {
        generation,
        request: "emit_test_rows",
        waits: stats.backpressure_waits,
    }];
    assert!(
        stats.backpressure_waits > 0,
        "slow sink should force the bounded reader buffer to report backpressure"
    );
    assert_eq!(stats.stream_successes, 1);
    assert_eq!(stats.stream_failures, 0);
    assert!(trace.contains(&RuntimeTraceEvent::BackpressureObserved {
        generation,
        request: "emit_test_rows",
        waits: stats.backpressure_waits,
    }));
    drop(worker);
    Ok(())
}

fn restart_limited_fake_worker(mode: &str, max_restarts: u64) -> Result<LeanWorker, LeanWorkerError> {
    fake_worker_with_config(mode, |config| {
        config.restart_policy(
            LeanWorkerRestartPolicy::default().max_restarts_per_window(max_restarts, Duration::from_mins(1)),
        )
    })
}

fn conformance_explicit_shutdown_gracefully_reaps_child() -> Result<(), String> {
    let worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose a child pid".to_owned())?;
    let mut trace = vec![
        RuntimeTraceEvent::GenerationStarted(generation),
        RuntimeTraceEvent::ShutdownStarted { generation },
        RuntimeTraceEvent::GracefulStopAttempted { generation },
    ];
    let report = worker.shutdown().map_err(|err| err.to_string())?;
    assert_eq!(report.outcome, LeanWorkerShutdownOutcome::Graceful);
    assert!(report.exit.success, "fake child should exit cleanly");
    trace.push(RuntimeTraceEvent::ChildReaped { generation });
    assert!(trace.contains(&RuntimeTraceEvent::ChildReaped { generation }));
    assert_reaped(pid)
}

fn conformance_dropped_idle_worker_reaps_child() -> Result<(), String> {
    let worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose a child pid".to_owned())?;
    let trace = [
        RuntimeTraceEvent::GenerationStarted(generation),
        RuntimeTraceEvent::ShutdownStarted { generation },
        RuntimeTraceEvent::GracefulStopAttempted { generation },
        RuntimeTraceEvent::ChildReaped { generation },
    ];
    drop(worker);
    assert!(trace.contains(&RuntimeTraceEvent::ChildReaped { generation }));
    assert_reaped(pid)
}

fn conformance_dropped_worker_escalates_kill_and_reaps_child() -> Result<(), String> {
    let worker = fake_worker("terminate_hang").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose a child pid".to_owned())?;
    let trace = [
        RuntimeTraceEvent::GenerationStarted(generation),
        RuntimeTraceEvent::ShutdownStarted { generation },
        RuntimeTraceEvent::GracefulStopAttempted { generation },
        RuntimeTraceEvent::KillEscalated { generation },
        RuntimeTraceEvent::ChildReaped { generation },
    ];
    drop(worker);
    assert!(trace.contains(&RuntimeTraceEvent::KillEscalated { generation }));
    assert_reaped(pid)
}

fn conformance_timeout_kill_reap_restarts_next_generation() -> Result<(), String> {
    let mut worker = fake_worker("request_hang").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let old_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose an initial child pid".to_owned())?;
    let mut trace = request_trace(generation, "health");
    let err = worker.health().expect_err("wedged health request should time out");
    match err {
        LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "health"),
        other => return Err(format!("expected timeout error, got {other:?}")),
    }
    trace.extend([
        RuntimeTraceEvent::TimeoutObserved {
            generation,
            request: "health",
        },
        RuntimeTraceEvent::TerminalOutcomeObserved {
            generation,
            request: "health",
            outcome: RuntimeTerminalOutcome::Timeout,
        },
        RuntimeTraceEvent::KillEscalated { generation },
        RuntimeTraceEvent::ChildReaped { generation },
    ]);
    assert_reaped(old_pid)?;
    let new_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not spawn a replacement child".to_owned())?;
    assert_ne!(old_pid, new_pid, "timeout should replace the wedged child");
    let replacement_generation = worker.lifecycle_snapshot().worker_generation;
    trace.push(RuntimeTraceEvent::RestartObserved {
        from: generation,
        to: replacement_generation,
    });
    assert_eq!(replacement_generation, generation.saturating_add(1));
    let stats = worker.stats();
    assert_eq!(stats.requests, 1, "accepted request should be recorded once");
    assert_eq!(stats.timeout_restarts, 1, "timeout should be terminalized once");
    assert_single_terminal(&trace, generation, "health")?;
    drop(worker);
    assert_reaped(new_pid)
}

fn conformance_child_crash_terminalizes_in_flight_request() -> Result<(), String> {
    let mut worker = fake_worker("crash_on_health").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose a child pid".to_owned())?;
    let mut trace = request_trace(generation, "health");
    let err = worker.health().expect_err("fake child crash should fail request");
    match err {
        LeanWorkerError::ChildPanicOrAbort { exit } => assert!(!exit.success),
        other => return Err(format!("expected child fatal exit, got {other:?}")),
    }
    trace.extend([
        RuntimeTraceEvent::ChildCrashObserved {
            generation,
            request: "health",
        },
        RuntimeTraceEvent::TerminalOutcomeObserved {
            generation,
            request: "health",
            outcome: RuntimeTerminalOutcome::ChildPanicOrAbort,
        },
        RuntimeTraceEvent::ChildReaped { generation },
    ]);
    assert_single_terminal(&trace, generation, "health")?;
    assert_reaped(pid)
}

fn conformance_restart_limit_exhaustion_is_typed_terminal_outcome() -> Result<(), String> {
    let mut worker = restart_limited_fake_worker("request_hang", 1).map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let first_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose an initial child pid".to_owned())?;
    let mut trace = request_trace(generation, "health");
    let err = worker
        .health()
        .expect_err("first wedged health request should time out");
    match err {
        LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "health"),
        other => return Err(format!("expected first timeout error, got {other:?}")),
    }
    trace.extend([
        RuntimeTraceEvent::TimeoutObserved {
            generation,
            request: "health",
        },
        RuntimeTraceEvent::TerminalOutcomeObserved {
            generation,
            request: "health",
            outcome: RuntimeTerminalOutcome::Timeout,
        },
        RuntimeTraceEvent::ChildReaped { generation },
    ]);
    assert_single_terminal(&trace, generation, "health")?;
    assert_reaped(first_pid)?;

    let replacement_generation = worker.lifecycle_snapshot().worker_generation;
    let second_pid = worker
        .__child_pid_for_test()
        .ok_or_else(|| "worker did not expose replacement child pid".to_owned())?;
    let mut replacement_trace = request_trace(replacement_generation, "health");
    let err = worker
        .health()
        .expect_err("second wedged health request should exhaust restart limit");
    match err {
        LeanWorkerError::RestartLimitExceeded { restarts, window } => {
            assert_eq!(restarts, 1);
            assert_eq!(window, Duration::from_mins(1));
        }
        other => return Err(format!("expected restart-limit error, got {other:?}")),
    }
    replacement_trace.extend([
        RuntimeTraceEvent::TimeoutObserved {
            generation: replacement_generation,
            request: "health",
        },
        RuntimeTraceEvent::RestartLimitExhausted {
            generation: replacement_generation,
        },
        RuntimeTraceEvent::TerminalOutcomeObserved {
            generation: replacement_generation,
            request: "health",
            outcome: RuntimeTerminalOutcome::RestartLimitExceeded,
        },
        RuntimeTraceEvent::ChildReaped {
            generation: replacement_generation,
        },
    ]);
    assert_single_terminal(&replacement_trace, replacement_generation, "health")?;
    assert_reaped(second_pid)?;

    let err = worker
        .health()
        .expect_err("restart-exhausted worker should stop accepting work");
    match err {
        LeanWorkerError::ShutdownInProgress { operation } => assert_eq!(operation, "worker_request"),
        other => return Err(format!("expected shutdown-in-progress after exhaustion, got {other:?}")),
    }
    let refused_trace = [RuntimeTraceEvent::TerminalOutcomeObserved {
        generation: replacement_generation,
        request: "health",
        outcome: RuntimeTerminalOutcome::ShutdownInProgress,
    }];
    assert_eq!(refused_trace.len(), 1);

    let stats = worker.stats();
    assert_eq!(
        stats.restarts, 1,
        "only the admitted replacement should bump generation"
    );
    assert_eq!(
        stats.timeout_restarts, 1,
        "only the admitted timeout replacement is counted"
    );
    assert_eq!(
        stats.replacement_attempts, 2,
        "both timeout replacements were attempted"
    );
    assert_eq!(stats.replacement_successes, 1);
    assert_eq!(stats.replacement_failures, 1);
    assert_eq!(stats.replacement_budget_admitted, 1);
    assert_eq!(stats.replacement_budget_skipped, 1);
    assert_eq!(
        stats.last_replacement_skipped_reason.as_deref(),
        Some("restart_limit_exceeded")
    );
    Ok(())
}

fn conformance_pool_lease_drop_releases_capacity_once() -> Result<(), String> {
    let fixture = FakeCapabilityFixture::new("lease-drop")?;
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let builder = fixture.builder("normal", ["Init"])?;

    let before = pool.snapshot();
    assert_eq!(before.active_workers, 0);
    assert_eq!(before.warm_leases, 0);

    {
        let lease = pool.acquire_lease(builder.clone()).map_err(|err| err.to_string())?;
        let snapshot = lease.snapshot();
        let trace = [RuntimeTraceEvent::LeaseGranted {
            active_workers: snapshot.active_workers,
            warm_leases: snapshot.warm_leases,
        }];
        assert_eq!(snapshot.active_workers, 1);
        assert_eq!(snapshot.warm_leases, 0);
        assert!(trace.contains(&RuntimeTraceEvent::LeaseGranted {
            active_workers: 1,
            warm_leases: 0,
        }));
    }

    let after_drop = pool.snapshot();
    let trace = [RuntimeTraceEvent::LeaseDropped {
        active_workers: after_drop.active_workers,
        warm_leases: after_drop.warm_leases,
    }];
    assert_eq!(after_drop.active_workers, 0);
    assert_eq!(after_drop.warm_leases, 1);
    assert!(trace.contains(&RuntimeTraceEvent::LeaseDropped {
        active_workers: 0,
        warm_leases: 1,
    }));

    {
        let lease = pool.acquire_lease(builder).map_err(|err| err.to_string())?;
        let snapshot = lease.snapshot();
        assert_eq!(snapshot.active_workers, 1);
        assert_eq!(
            snapshot.workers, 1,
            "dropping the first lease should not leave a second active accounting slot"
        );
    }
    assert_eq!(pool.snapshot().key_hits, 1);

    Ok(())
}

fn conformance_pool_explicit_release_decrements_capacity_once() -> Result<(), String> {
    let fixture = FakeCapabilityFixture::new("lease-release")?;
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let builder = fixture.builder("normal", ["Init"])?;

    {
        let lease = pool.acquire_lease(builder).map_err(|err| err.to_string())?;
        let snapshot = lease.snapshot();
        assert_eq!(snapshot.active_workers, 1);
        assert_eq!(snapshot.warm_leases, 0);
        lease.release();
    }

    let after_release = pool.snapshot();
    let trace = [RuntimeTraceEvent::LeaseReleased {
        active_workers: after_release.active_workers,
        warm_leases: after_release.warm_leases,
    }];
    assert_eq!(after_release.active_workers, 0);
    assert_eq!(after_release.warm_leases, 1);
    assert!(trace.contains(&RuntimeTraceEvent::LeaseReleased {
        active_workers: 0,
        warm_leases: 1,
    }));
    Ok(())
}

fn conformance_pool_idle_replacement_preserves_capacity_accounting() -> Result<(), String> {
    let fixture = FakeCapabilityFixture::new("idle-replacement")?;
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1).idle_cycle_after(Duration::ZERO));
    let builder = fixture.builder("normal", ["Init"])?;

    {
        let lease = pool.acquire_lease(builder.clone()).map_err(|err| err.to_string())?;
        let snapshot = lease.snapshot();
        assert_eq!(snapshot.active_workers, 1);
        assert_eq!(snapshot.worker_restarts, 0);
    }

    let warm_snapshot = pool.snapshot();
    assert_eq!(warm_snapshot.active_workers, 0);
    assert_eq!(warm_snapshot.warm_leases, 1);

    {
        let lease = pool.acquire_lease(builder).map_err(|err| err.to_string())?;
        let snapshot = lease.snapshot();
        assert_eq!(snapshot.active_workers, 1);
        assert_eq!(snapshot.workers, 1);
        assert_eq!(snapshot.policy_restarts, 1);
        assert_eq!(snapshot.worker_restarts, 1);
        assert_eq!(snapshot.idle_restarts, 1);
        let trace = [RuntimeTraceEvent::IdleReplacementObserved {
            policy_restarts: snapshot.policy_restarts,
            worker_restarts: snapshot.worker_restarts,
        }];
        assert!(trace.contains(&RuntimeTraceEvent::IdleReplacementObserved {
            policy_restarts: 1,
            worker_restarts: 1,
        }));
    }

    let after_drop = pool.snapshot();
    assert_eq!(after_drop.active_workers, 0);
    assert_eq!(after_drop.warm_leases, 1);
    assert_eq!(after_drop.policy_restarts, 1);
    Ok(())
}

fn conformance_pool_admission_refusal_is_explicit() -> Result<(), String> {
    let fixture = FakeCapabilityFixture::new("admission-refusal")?;
    let mut pool = LeanWorkerPool::new(LeanWorkerPoolConfig::new(1));
    let first = fixture.builder("normal", ["Init"])?;
    let second = fixture.builder("normal", ["Std"])?;

    {
        let first_lease = pool.acquire_lease(first).map_err(|err| err.to_string())?;
        assert_eq!(first_lease.snapshot().workers, 1);
    }
    let err = pool
        .acquire_lease(second)
        .expect_err("distinct session key should be refused when capacity is full");
    match err {
        LeanWorkerError::WorkerPoolExhausted { max_workers, resource } => {
            assert_eq!(max_workers, 1);
            assert_eq!(resource.cause, "worker_pool_max_workers");
            assert!(!resource.work_entered_child);
        }
        other => return Err(format!("expected explicit pool exhaustion, got {other:?}")),
    }
    let snapshot = pool.snapshot();
    let trace = [RuntimeTraceEvent::AdmissionRefused { reason: "max_workers" }];
    assert_eq!(snapshot.cold_open_refusals, 1);
    assert_eq!(snapshot.refusal_reason.as_deref(), Some("max_workers"));
    assert!(trace.contains(&RuntimeTraceEvent::AdmissionRefused { reason: "max_workers" }));
    Ok(())
}

fn declaration_outline_batch_selector_reaches_parent_session_path() -> Result<(), String> {
    let fixture = FakeCapabilityFixture::new("declaration-outline")?;
    let mut capability = fixture
        .builder("normal", ["Init"])?
        .open()
        .map_err(|err| err.to_string())?;
    let mut session = capability.open_session(None, None).map_err(|err| err.to_string())?;

    let outcome = session
        .process_module_query_batch(
            "theorem outlined : True := by\n  trivial\n",
            &[LeanWorkerModuleQuerySelector::DeclarationOutline {
                id: "outline".to_owned(),
            }],
            &LeanWorkerOutputBudgets::default(),
            &LeanWorkerElabOptions::default(),
            None,
            None,
        )
        .map_err(|err| err.to_string())?;

    let LeanWorkerModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        return Err(format!("expected Ok declaration-outline outcome, got {outcome:?}"));
    };
    let [LeanWorkerModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        return Err(format!(
            "expected one Ok declaration-outline item, got {:?}",
            result.items
        ));
    };
    let LeanWorkerModuleQueryBatchResult::DeclarationOutline(outline) = result.as_ref() else {
        return Err(format!("expected declaration-outline result, got {result:?}"));
    };
    assert_eq!(outline.declarations.len(), 1);
    let declaration = outline
        .declarations
        .first()
        .ok_or_else(|| "missing declaration-outline row".to_owned())?;
    assert_eq!(declaration.declaration_name, "Fake.outlined");
    assert!(!outline.truncated);
    Ok(())
}

fn declaration_verification_batch_reaches_parent_session_path() -> Result<(), String> {
    let fixture = FakeCapabilityFixture::new("declaration-verification-batch")?;
    let mut capability = fixture
        .builder("normal", ["Init"])?
        .open()
        .map_err(|err| err.to_string())?;
    let mut session = capability.open_session(None, None).map_err(|err| err.to_string())?;

    let request = LeanWorkerDeclarationVerificationBatchRequest {
        source: "theorem checked : True := by\n  trivial\n".to_owned(),
        targets: vec![LeanWorkerDeclarationVerificationBatchItem {
            id: "checked-row".to_owned(),
            target: LeanWorkerDeclarationVerificationTarget::Name {
                name: "checked".to_owned(),
            },
        }],
        sorry_policy: LeanWorkerSorryPolicy::Deny,
        report_axioms: true,
        budgets: LeanWorkerOutputBudgets::default(),
    };

    let outcome = session
        .verify_declaration_batch(&request, &LeanWorkerElabOptions::default(), None, None)
        .map_err(|err| err.to_string())?;
    let LeanWorkerDeclarationVerificationBatchResult::Ok { results, imports } = outcome else {
        return Err(format!(
            "expected Ok declaration-verification batch outcome, got {outcome:?}"
        ));
    };
    assert!(imports.is_empty());
    let [row] = results.as_slice() else {
        return Err(format!("expected one verification row, got {results:?}"));
    };
    assert_eq!(row.id, "checked-row");
    assert_eq!(
        row.verification_status,
        LeanWorkerDeclarationVerificationStatus::Accepted
    );
    assert!(
        !row.facts.axioms_available,
        "fake child uses unavailable facts for transport-only verification"
    );
    Ok(())
}

fn command_message_diagnostics_selector_reaches_parent_session_path() -> Result<(), String> {
    let fixture = FakeCapabilityFixture::new("command-message")?;
    let mut capability = fixture
        .builder("normal", ["Init"])?
        .open()
        .map_err(|err| err.to_string())?;
    let mut session = capability.open_session(None, None).map_err(|err| err.to_string())?;

    let outcome = session
        .process_module_query_batch(
            "#check Nat.add\n",
            &[LeanWorkerModuleQuerySelector::Diagnostics {
                id: "messages".to_owned(),
            }],
            &LeanWorkerOutputBudgets::default(),
            &LeanWorkerElabOptions::default(),
            None,
            None,
        )
        .map_err(|err| err.to_string())?;

    let LeanWorkerModuleQueryBatchOutcome::Ok { result, imports, .. } = outcome else {
        return Err(format!("expected Ok command-message outcome, got {outcome:?}"));
    };
    assert!(imports.is_empty());
    let [LeanWorkerModuleQueryBatchItem::Ok { result, .. }] = result.items.as_slice() else {
        return Err(format!("expected one diagnostics item, got {:?}", result.items));
    };
    let LeanWorkerModuleQueryBatchResult::Diagnostics(diagnostics) = result.as_ref() else {
        return Err(format!("expected diagnostics result, got {result:?}"));
    };
    assert!(
        diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "info" && diagnostic.message.contains("Nat.add")),
        "fake command-message diagnostics should cross the parent session path, got {diagnostics:?}",
    );
    Ok(())
}

struct FakeCapabilityFixture {
    root: PathBuf,
    manifest: PathBuf,
}

impl FakeCapabilityFixture {
    fn new(name: &str) -> Result<Self, String> {
        let root = env::temp_dir().join(format!(
            "lean-rs-worker-parent-{name}-{}-{}",
            std::process::id(),
            thread_id_suffix()
        ));
        let lib_dir = root.join(".lake").join("build").join("lib");
        std::fs::create_dir_all(&lib_dir).map_err(|err| format!("create fake capability lib dir: {err}"))?;
        let dylib = lib_dir.join(if cfg!(target_os = "macos") {
            "libFakeConformance.dylib"
        } else {
            "libFakeConformance.so"
        });
        std::fs::write(&dylib, b"fake dylib placeholder")
            .map_err(|err| format!("write fake capability dylib: {err}"))?;
        let manifest = root.join("fake-capability.json");
        std::fs::write(
            &manifest,
            format!(
                r#"{{"schema_version":2,"primary_dylib":{},"package":"fake_conformance","module":"FakeConformance","exports":[]}}"#,
                serde_json::to_string(&dylib).map_err(|err| format!("encode fake dylib path: {err}"))?
            ),
        )
        .map_err(|err| format!("write fake capability manifest: {err}"))?;
        Ok(Self { root, manifest })
    }

    fn builder<const N: usize>(&self, mode: &str, imports: [&str; N]) -> Result<LeanWorkerCapabilityBuilder, String> {
        let executable = self.fake_child_wrapper(mode)?;
        LeanWorkerCapabilityBuilder::from_built_capability(&LeanBuiltCapability::manifest_path(&self.manifest), imports)
            .map_err(|err| err.to_string())
            .map(|builder| {
                builder
                    .worker_child(LeanWorkerChild::for_toolchain(executable, &self.root))
                    .startup_timeout(Duration::from_secs(1))
                    .request_timeout(Duration::from_millis(80))
                    .shutdown_timeout(Duration::from_millis(80))
            })
    }

    fn fake_child_wrapper(&self, mode: &str) -> Result<PathBuf, String> {
        let current_exe = env::current_exe().map_err(|err| format!("resolve current test executable: {err}"))?;
        let wrapper = self.root.join(wrapper_name(mode));
        write_fake_child_wrapper(&wrapper, &current_exe, mode)?;
        Ok(wrapper)
    }
}

impl Drop for FakeCapabilityFixture {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.root));
    }
}

fn thread_id_suffix() -> String {
    format!("{:?}", thread::current().id())
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn wrapper_name(mode: &str) -> String {
    let mode = mode
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .collect::<String>();
    if cfg!(windows) {
        format!("fake-worker-{mode}.cmd")
    } else {
        format!("fake-worker-{mode}.sh")
    }
}

#[cfg(unix)]
fn write_fake_child_wrapper(
    wrapper: &std::path::Path,
    current_exe: &std::path::Path,
    mode: &str,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let script = format!(
        "#!/bin/sh\n{}={} exec {} \"$@\"\n",
        FAKE_CHILD_ENV,
        shell_quote(mode),
        shell_quote_path(current_exe)
    );
    std::fs::write(wrapper, script).map_err(|err| format!("write fake child wrapper: {err}"))?;
    let mut permissions = std::fs::metadata(wrapper)
        .map_err(|err| format!("stat fake child wrapper: {err}"))?
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(wrapper, permissions).map_err(|err| format!("chmod fake child wrapper: {err}"))
}

#[cfg(windows)]
fn write_fake_child_wrapper(
    wrapper: &std::path::Path,
    current_exe: &std::path::Path,
    mode: &str,
) -> Result<(), String> {
    let script = format!(
        "@echo off\r\nset {}={}\r\n\"{}\" %*\r\n",
        FAKE_CHILD_ENV,
        mode,
        current_exe.display()
    );
    std::fs::write(wrapper, script).map_err(|err| format!("write fake child wrapper: {err}"))
}

#[cfg(unix)]
fn shell_quote_path(path: &std::path::Path) -> String {
    shell_quote(&path.to_string_lossy())
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Default)]
struct RecordingSink(std::sync::Mutex<Vec<LeanWorkerDataRow>>);

impl RecordingSink {
    fn rows(&self) -> Vec<LeanWorkerDataRow> {
        self.0.lock().expect("recording sink mutex is not poisoned").clone()
    }
}

impl LeanWorkerDataSink for RecordingSink {
    fn report(&self, row: LeanWorkerDataRow) {
        self.0.lock().expect("recording sink mutex is not poisoned").push(row);
    }
}

struct CancellingSink {
    rows: std::sync::Mutex<Vec<LeanWorkerDataRow>>,
    cancellation: LeanWorkerCancellationToken,
}

impl CancellingSink {
    fn new(cancellation: LeanWorkerCancellationToken) -> Self {
        Self {
            rows: std::sync::Mutex::new(Vec::new()),
            cancellation,
        }
    }

    fn rows(&self) -> Vec<LeanWorkerDataRow> {
        self.rows.lock().expect("cancelling sink mutex is not poisoned").clone()
    }
}

impl LeanWorkerDataSink for CancellingSink {
    fn report(&self, row: LeanWorkerDataRow) {
        {
            self.rows
                .lock()
                .expect("cancelling sink mutex is not poisoned")
                .push(row);
        }
        self.cancellation.cancel();
    }
}

struct SlowSink {
    rows: std::sync::Mutex<Vec<LeanWorkerDataRow>>,
    delay: Duration,
}

impl SlowSink {
    fn new(delay: Duration) -> Self {
        Self {
            rows: std::sync::Mutex::new(Vec::new()),
            delay,
        }
    }

    fn rows(&self) -> Vec<LeanWorkerDataRow> {
        self.rows.lock().expect("slow sink mutex is not poisoned").clone()
    }
}

impl LeanWorkerDataSink for SlowSink {
    fn report(&self, row: LeanWorkerDataRow) {
        thread::sleep(self.delay);
        self.rows.lock().expect("slow sink mutex is not poisoned").push(row);
    }
}

fn assert_reaped(pid: u32) -> Result<(), String> {
    for _ in 0..100 {
        match process_state(pid)? {
            ProcessState::Missing => return Ok(()),
            ProcessState::Zombie => return Err(format!("child process {pid} is a zombie")),
            ProcessState::Alive => thread::sleep(Duration::from_millis(10)),
            #[cfg(not(unix))]
            ProcessState::Unknown => return Ok(()),
        }
    }
    Err(format!("child process {pid} still exists after cleanup"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessState {
    Alive,
    Missing,
    Zombie,
    #[cfg(not(unix))]
    Unknown,
}

#[cfg(target_os = "linux")]
fn process_state(pid: u32) -> Result<ProcessState, String> {
    match std::fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(status) => {
            if status
                .lines()
                .any(|line| line.starts_with("State:") && line.contains('Z'))
            {
                Ok(ProcessState::Zombie)
            } else {
                Ok(ProcessState::Alive)
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(ProcessState::Missing),
        Err(err) => Err(format!("failed to inspect /proc/{pid}/status: {err}")),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_state(pid: u32) -> Result<ProcessState, String> {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map_err(|err| format!("failed to run ps for pid {pid}: {err}"))?;
    if !output.status.success() {
        return Ok(ProcessState::Missing);
    }
    let stat = String::from_utf8_lossy(&output.stdout);
    if stat.trim().is_empty() {
        Ok(ProcessState::Missing)
    } else if stat.contains('Z') {
        Ok(ProcessState::Zombie)
    } else {
        Ok(ProcessState::Alive)
    }
}

#[cfg(not(unix))]
fn process_state(_pid: u32) -> Result<ProcessState, String> {
    Ok(ProcessState::Unknown)
}
