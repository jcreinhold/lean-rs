#![allow(
    clippy::exit,
    clippy::expect_used,
    clippy::panic,
    clippy::unnecessary_wraps,
    clippy::wildcard_enum_match_arm
)]

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
use std::process::Command;

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerCancellationToken, LeanWorkerChild, LeanWorkerConfig,
    LeanWorkerDeclarationVerificationBatchItem, LeanWorkerDeclarationVerificationBatchRequest,
    LeanWorkerDeclarationVerificationTarget, LeanWorkerError, LeanWorkerHostHandleBuilder,
    LeanWorkerHostRuntimeExpectation, LeanWorkerHostRuntimeMetadata, LeanWorkerProgressEvent, LeanWorkerProgressSink,
    LeanWorkerRestartPolicy, LeanWorkerRestartReason, LeanWorkerSession, LeanWorkerSessionConfig,
    LeanWorkerShutdownOutcome, LeanWorkerSorryPolicy,
};
use lean_rs_worker_protocol::protocol::{
    DataRowEmitter, MAX_FRAME_BYTES, Message, PROTOCOL_VERSION, ProgressTick, Request, Response, read_frame,
    write_frame,
};
use lean_rs_worker_protocol::types::{
    LeanWorkerDeclarationFilter, LeanWorkerDeclarationOutlineResult, LeanWorkerDeclarationTargetInfo,
    LeanWorkerDeclarationVerificationBatchResult,
    LeanWorkerDeclarationVerificationBatchRow, LeanWorkerDeclarationVerificationFacts,
    LeanWorkerDeclarationVerificationStatus, LeanWorkerDiagnostic, LeanWorkerElabFailure, LeanWorkerElabOptions,
    LeanWorkerImportStats, LeanWorkerModuleCacheStatus, LeanWorkerModuleQueryBatchEnvelope,
    LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryBatchResult,
    LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQuerySelector, LeanWorkerModuleQueryTimings,
    LeanWorkerModuleSourceSpan, LeanWorkerOutputBudgets, LeanWorkerSessionImportProfile,
    LeanWorkerSourceCoordinateSpace, LeanWorkerSourceRange,
};
use serde_json::value::RawValue;

const FAKE_CHILD_ENV: &str = "LEAN_RS_WORKER_PARENT_FAKE_CHILD";

/// Unreclaimable bytes the `residue_reporting` fake child attributes to each
/// import. Small enough to keep the arithmetic in the tests legible; the policy
/// only ever compares a running sum against a limit, so the magnitude is
/// irrelevant to what is being pinned.
const FAKE_IMPORT_RESIDUE_BYTES: u64 = 1_000;

// Import-light conformance harness for the worker-parent runtime model.
//
// The test binary re-enters itself as a fake worker child through `FAKE_CHILD_ENV`, speaks
// deterministic worker-protocol frames, and never opens a Lean session.
// `RuntimeTraceEvent` below names the model-level observations these tests protect. Real
// Lean integration still belongs in the worker-child tests. Exact command:
//
//   cargo nextest run -p lean-rs-worker-parent --profile ci
//
const LAUNCH_INPUT_CASE: &str = "worker_application_launch_input";
const INPUT_ENV: &str = "LEAN_RS_WORKER_APPLICATION_INPUT";

fn main() {
    if env::args().nth(1).as_deref() == Some("--test-launch-input-parent") {
        check_application_launch_input();
        return;
    }
    if let Ok(mode) = env::var(FAKE_CHILD_ENV) {
        run_fake_child(&mode);
        return;
    }
    const CASE: &str = "worker_shutdown_conformance";
    let args: Vec<_> = env::args().skip(1).collect();
    let mut args = args.iter().map(String::as_str);
    let mut listing = false;
    let mut ignored = false;
    let mut selected = None;
    while let Some(arg) = args.next() {
        match arg {
            "--list" => listing = true,
            "--ignored" => ignored = true,
            "--format" if args.next() == Some("terse") => {}
            "--format=terse" | "--exact" | "--nocapture" | "--quiet" => {}
            CASE | LAUNCH_INPUT_CASE if selected.is_none() => selected = Some(arg),
            _ => panic!("unsupported worker shutdown test argument: {arg}"),
        }
    }
    if listing {
        if !ignored {
            println!("{CASE}: test");
            println!("{LAUNCH_INPUT_CASE}: test");
        }
        return;
    }
    if ignored {
        return;
    }

    if selected != Some(CASE) {
        // Use a fresh parent environment without mutating the test process.
        let status = std::process::Command::new(env::current_exe().expect("test executable"))
            .arg("--test-launch-input-parent")
            // The self-contained fake parent needs none of the test runner's loader overrides.
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LD_PRELOAD")
            .env_remove("DYLD_LIBRARY_PATH")
            .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
            .env_remove("DYLD_INSERT_LIBRARIES")
            .env(INPUT_ENV, "ambient")
            .env(FAKE_CHILD_ENV, "launch_input")
            .status()
            .expect("isolated application input test parent");
        assert!(status.success());
        if selected == Some(LAUNCH_INPUT_CASE) {
            return;
        }
    }

    public_worker_handles_keep_unwind_safety();
    let tests: &[(&str, fn() -> Result<(), String>)] = &[
        (
            "terminal_refusal_survives_until_later_dispatch",
            terminal_refusal_survives_until_later_dispatch,
        ),
        (
            "artifact_epoch_change_recycles_all_pooled_state_before_body",
            artifact_epoch_change_recycles_all_pooled_state_before_body,
        ),
        (
            "host_metadata_is_checked_before_session_is_exposed",
            host_metadata_is_checked_before_session_is_exposed,
        ),
        (
            "host_metadata_is_revalidated_after_recycle",
            host_metadata_is_revalidated_after_recycle,
        ),
        (
            "host_metadata_accepts_compatible_extra_bindings",
            host_metadata_accepts_compatible_extra_bindings,
        ),
        (
            "host_metadata_unobserved_protocol_failure_has_no_terminal_refusal",
            host_metadata_unobserved_protocol_failure_has_no_terminal_refusal,
        ),
        (
            "host_handle_requires_host_metadata_before_first_session",
            host_handle_requires_host_metadata_before_first_session,
        ),
        (
            "host_handle_revalidates_host_metadata_after_replacement",
            host_handle_revalidates_host_metadata_after_replacement,
        ),
        (
            "stale_warm_session_keeps_import_count_backstop",
            stale_warm_session_keeps_import_count_backstop,
        ),
        (
            "failed_header_then_name_listing_reopens_session",
            failed_header_then_name_listing_reopens_session,
        ),
        (
            "actual_session_miss_recycles_once_but_warm_reuse_does_not",
            actual_session_miss_recycles_once_but_warm_reuse_does_not,
        ),
        (
            "send_and_response_share_deadline_and_replacement_remains_usable",
            send_and_response_share_deadline_and_replacement_remains_usable,
        ),
        (
            "conformance_terminal_success_has_one_terminal_outcome",
            conformance_terminal_success_has_one_terminal_outcome,
        ),
        (
            "conformance_name_rows_complete_with_one_terminal_outcome",
            conformance_name_rows_complete_with_one_terminal_outcome,
        ),
        (
            "conformance_name_rows_then_child_exit_fail_the_request",
            conformance_name_rows_then_child_exit_fail_the_request,
        ),
        (
            "conformance_name_rows_then_child_abort_fail_the_request",
            conformance_name_rows_then_child_abort_fail_the_request,
        ),
        (
            "conformance_name_rows_then_silence_time_out",
            conformance_name_rows_then_silence_time_out,
        ),
        (
            "conformance_silent_worker_cancellation_reaps_only_owned_child",
            conformance_silent_worker_cancellation_reaps_only_owned_child,
        ),
        (
            "conformance_silent_worker_deadline_is_not_renewed_by_polling",
            conformance_silent_worker_deadline_is_not_renewed_by_polling,
        ),
        (
            "conformance_slow_progress_sink_backpressure_is_bounded_and_observable",
            conformance_slow_progress_sink_backpressure_is_bounded_and_observable,
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
            "request_stop_receipt_matches_checkpoint_across_replacement",
            request_stop_receipt_matches_checkpoint_across_replacement,
        ),
        (
            "newer_request_shadows_older_stop_receipt",
            newer_request_shadows_older_stop_receipt,
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
            "import_residue_accumulates_over_a_generation_and_resets_on_the_cycle",
            import_residue_accumulates_over_a_generation_and_resets_on_the_cycle,
        ),
        (
            "a_child_that_reports_no_region_bytes_is_counted_and_still_bounded",
            a_child_that_reports_no_region_bytes_is_counted_and_still_bounded,
        ),
        (
            "header_budget_reuses_warm_and_retries_distinct_once",
            header_budget_reuses_warm_and_retries_distinct_once,
        ),
        (
            "failed_header_preserves_result_and_retires_next_request",
            failed_header_preserves_result_and_retires_next_request,
        ),
        (
            "header_recycle_is_retried_only_once",
            header_recycle_is_retried_only_once,
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

fn check_application_launch_input() {

    assert_eq!(env::var(INPUT_ENV).as_deref(), Ok("ambient"));
    let executable = env::current_exe().expect("fixture executable");
    let root = env::current_dir().expect("fixture root");
    let locator = LeanWorkerChild::for_toolchain(executable, &root);
    assert!(locator.clone().application_launch_input(&"é".repeat(2048)).is_ok());
    for invalid in ["", "embedded\0byte", &"é".repeat(2049)] {
        assert!(matches!(
            locator.clone().application_launch_input(invalid),
            Err(LeanWorkerError::Setup { .. })
        ));
    }

    const DATA: &str = "deployment Δ\nKEY=value";
    for (child, expected) in [
        (locator.clone(), None),
        (locator.clone().checked_native_loader_environment(), None),
        (
            locator
                .application_launch_input(DATA)
                .expect("valid input")
                .checked_native_loader_environment(),
            Some(DATA),
        ),
    ] {
        let mut handle = LeanWorkerHostHandleBuilder::shims_only(&root, std::iter::empty::<&str>())
            .worker_child(child)
            .open()
            .expect("framed worker started");
        let observe = |metadata: lean_rs_worker_parent::LeanWorkerRuntimeMetadata| -> (u32, Option<String>) {
            serde_json::from_str(&metadata.worker_version).expect("child reported pid and launch value")
        };
        let first = observe(handle.runtime_metadata());
        assert_eq!(first.1.as_deref(), expected);
        handle.restart().expect("worker replaced");
        let replacement = observe(handle.runtime_metadata());
        assert_ne!(first.0, replacement.0);
        assert_eq!(replacement.1.as_deref(), expected);
        handle.try_shutdown().expect("replacement shut down");
    }
}

fn run_fake_child(mode: &str) {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    write_frame(
        &mut stdout,
        Message::Handshake {
            worker_version: if mode == "launch_input" {
                serde_json::to_string(&(std::process::id(), env::var(INPUT_ENV).ok())).expect("child observation")
            } else {
                "fake-worker-shutdown-test".to_owned()
            },
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

    let mut last_header: Option<String> = None;
    let mut last_session: Option<Vec<String>> = None;
    let mut reused_session = false;
    let mut host_open_count = 0_u64;
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
            Request::OpenHostSession {
                imports,
                import_profile,
                ..
            } => {
                host_open_count = host_open_count.saturating_add(1);
                if mode == "refuse_once" && host_open_count == 1 {
                    write_frame(
                        &mut stdout,
                        Message::Response(Response::Error {
                            code: "fake.import_refused".to_owned(),
                            message: "original framed import refusal".to_owned(),
                        }),
                        MAX_FRAME_BYTES,
                    )
                    .expect("fake terminal import refusal");
                    continue;
                }
                if mode == "artifact_epoch" && host_open_count == 2 {
                    write_frame(
                        &mut stdout,
                        Message::Response(Response::HostArtifactEpochChanged),
                        MAX_FRAME_BYTES,
                    )
                    .expect("explicit artifact epoch refusal before import/body");
                    continue;
                }
                if mode == "session_count" {
                    let response = if last_session.is_none() {
                        last_session = Some(imports.clone());
                        Response::HostSessionOpened {
                            import_stats: fake_import_stats(imports, import_profile),
                        }
                    } else if !reused_session {
                        reused_session = true;
                        Response::HostSessionReused {
                            import_stats: fake_import_stats(imports, import_profile),
                        }
                    } else {
                        Response::HeaderImportRecycle
                    };
                    write_frame(&mut stdout, Message::Response(response), MAX_FRAME_BYTES)
                        .expect("session count response");
                    continue;
                }
                if mode == "session_budget" {
                    let response = if last_session.as_ref() == Some(&imports) {
                        Response::HostSessionReused {
                            import_stats: fake_import_stats(imports, import_profile),
                        }
                    } else if last_session.is_some() {
                        Response::HeaderImportRecycle
                    } else {
                        last_session = Some(imports.clone());
                        Response::HostSessionOpened {
                            import_stats: fake_import_stats(imports, import_profile),
                        }
                    };
                    write_frame(&mut stdout, Message::Response(response), MAX_FRAME_BYTES)
                        .expect("session budget response");
                    continue;
                }
                last_session = Some(imports.clone());
                let mut import_stats = fake_import_stats(imports, import_profile);
                if mode == "residue_reporting" {
                    // A child whose shim *does* attribute region bytes. The
                    // default fake stats are the opposite case — modules in,
                    // zero bytes out — which is what the unusable counter is for.
                    import_stats.compacted_region_bytes = FAKE_IMPORT_RESIDUE_BYTES;
                    import_stats.non_memory_mapped_region_bytes = FAKE_IMPORT_RESIDUE_BYTES;
                    import_stats.imported_bytes = FAKE_IMPORT_RESIDUE_BYTES;
                }
                write_frame(
                    &mut stdout,
                    Message::Response(Response::HostSessionOpened { import_stats }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake child writes session-open response");
                if mode == "slow_send_and_response" {
                    // Stop reading for a while, so the parent's next large
                    // request fills the pipe and its send must wait.
                    thread::sleep(Duration::from_millis(200));
                }
            }
            Request::ProcessModuleQueryBatch { .. } if mode == "query_hang" => sleep_forever(),
            Request::ProcessModuleQueryBatch { .. } if mode == "progress_then_hang" => {
                write_progress(&mut stdout, 1);
                sleep_forever();
            }
            Request::ProcessModuleQueryBatch { source, selectors, .. } => {
                match mode {
                    "slow_send_and_response" => thread::sleep(Duration::from_millis(200)),
                    "many_progress" => {
                        // Four times the parent reader's 64-event buffer.
                        for current in 0..256 {
                            write_progress(&mut stdout, current);
                        }
                    }
                    _ => {}
                }
                if mode.starts_with("header_") {
                    let recycle = mode == "header_always_recycle"
                        || (mode == "header_budget" && last_header.as_ref().is_some_and(|old| old != &source));
                    write_frame(
                        &mut stdout,
                        Message::HeaderImportState {
                            bytes: 2000,
                            imports: 1,
                            accounting_unknown: source == "bad",
                        },
                        MAX_FRAME_BYTES,
                    )
                    .expect("write header accounting");
                    if recycle {
                        write_frame(
                            &mut stdout,
                            Message::Response(Response::HeaderImportRecycle),
                            MAX_FRAME_BYTES,
                        )
                        .expect("write recycle response");
                        continue;
                    }
                    if source == "bad" {
                        write_frame(
                            &mut stdout,
                            Message::Response(Response::Error {
                                code: "test.invalid_import".to_owned(),
                                message: "original bad import diagnostic".to_owned(),
                            }),
                            MAX_FRAME_BYTES,
                        )
                        .expect("write original import error");
                        continue;
                    }
                    last_header = Some(source);
                }

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
            Request::ListDeclarationsStrings { .. } if mode == "header_failure" => {
                let response = if last_session.is_none() {
                    Response::Error {
                        code: "test.session_missing".to_owned(),
                        message: "must reopen active session".to_owned(),
                    }
                } else {
                    Response::RowsComplete { count: 0 }
                };
                write_frame(&mut stdout, Message::Response(response), MAX_FRAME_BYTES)
                    .expect("write name listing response");
            }
            Request::ListDeclarationsStrings { .. } => {
                let names: &[&str] = match mode {
                    "names_then_exit" | "names_then_abort" | "names_then_hang" => &["Fake.first"],
                    _ => &["Fake.first", "Fake.second", "Fake.third"],
                };
                let mut emitter = DataRowEmitter::default();
                for name in names {
                    let payload = RawValue::from_string(serde_json::to_string(name).expect("encode fake name"))
                        .expect("fake child builds JSON row payload");
                    let row = emitter.next("names", payload);
                    write_frame(&mut stdout, Message::DataRow(row), MAX_FRAME_BYTES)
                        .expect("fake child writes data row");
                }
                match mode {
                    "names_then_exit" => return,
                    "names_then_abort" => std::process::abort(),
                    "names_then_hang" => sleep_forever(),
                    _ => write_frame(
                        &mut stdout,
                        Message::Response(Response::RowsComplete { count: emitter.count() }),
                        MAX_FRAME_BYTES,
                    )
                    .expect("fake child writes row terminal response"),
                }
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
            Request::CapabilityMetadata { .. } if mode.starts_with("host_metadata_") => {
                if let Some(root) = env::var_os("LEAN_SYSROOT") {
                    std::fs::write(PathBuf::from(root).join("capability-export-dispatched"), b"dispatched")
                        .expect("record fake capability export body");
                }
                write_frame(
                    &mut stdout,
                    Message::Response(Response::CapabilityMetadata {
                        metadata: lean_rs_worker_protocol::types::LeanWorkerCapabilityMetadata {
                            commands: Vec::new(),
                            capabilities: Vec::new(),
                            lean_version: None,
                            extra: None,
                        },
                    }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake capability metadata body response");
            }
            Request::HostRuntimeMetadata => {
                if mode == "host_metadata_wrong_response" {
                    write_frame(&mut stdout, Message::Response(Response::HealthOk), MAX_FRAME_BYTES)
                        .expect("fake unobserved metadata response");
                    continue;
                }
                let fingerprint = if mode == "host_metadata_bad" { "c" } else { "a" };
                let metadata = LeanWorkerHostRuntimeMetadata {
                    compiled_lean_version: "4.35.0".into(),
                    compiled_header_sha256: "b".repeat(64),
                    runtime_lean_version: Some("4.35.0".into()),
                    checked_host_bindings: std::collections::BTreeMap::from([
                        ("verify_declaration_batch".into(), fingerprint.repeat(64)),
                        ("new_optional_binding".into(), "d".repeat(64)),
                    ]),
                };
                write_frame(
                    &mut stdout,
                    Message::Response(Response::HostRuntimeMetadata { metadata }),
                    MAX_FRAME_BYTES,
                )
                .expect("fake metadata response");
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
            coordinate_space: LeanWorkerSourceCoordinateSpace::OriginalSource,
            original_range: Some(LeanWorkerSourceRange {
                file: "/fake-command-message.lean".to_owned(),
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 1,
            }),
        }],
        truncated: false,
    }
}

fn sleep_forever() -> ! {
    loop {
        thread::sleep(Duration::from_mins(1));
    }
}

fn write_progress(stdout: &mut impl Write, current: u64) {
    write_frame(
        stdout,
        Message::ProgressTick(ProgressTick::new("fake", current, None)),
        MAX_FRAME_BYTES,
    )
    .expect("fake child writes progress tick");
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
    TerminalOutcomeObserved {
        generation: u64,
        request: &'static str,
        outcome: RuntimeTerminalOutcome,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeTerminalOutcome {
    Response(&'static str),
    Timeout,
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

fn shims_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::shims_only("/fake-project", ["Init"])
}

fn query_batch(
    session: &mut LeanWorkerSession<'_>,
    source: &str,
    cancellation: Option<&LeanWorkerCancellationToken>,
    progress: Option<&dyn LeanWorkerProgressSink>,
) -> Result<LeanWorkerModuleQueryBatchOutcome, LeanWorkerError> {
    session.process_module_query_batch(
        source,
        &[],
        &LeanWorkerOutputBudgets::default(),
        &LeanWorkerElabOptions::default(),
        cancellation,
        progress,
    )
}

fn list_names(
    worker: &mut LeanWorker,
    cancellation: Option<&LeanWorkerCancellationToken>,
) -> Result<Vec<String>, LeanWorkerError> {
    let mut session = worker.open_session(&shims_config(), None, None)?;
    session.list_declarations_strings(&LeanWorkerDeclarationFilter::default(), cancellation, None)
}

fn send_and_response_share_deadline_and_replacement_remains_usable() -> Result<(), String> {
    let mut worker = fake_worker_with_config("slow_send_and_response", |config| {
        config.request_timeout(Duration::from_millis(300))
    })
    .map_err(|error| error.to_string())?;
    let result = {
        let mut session = worker
            .open_session(&shims_config(), None, None)
            .map_err(|error| error.to_string())?;
        // Larger than the pipe buffer, so sending waits for the child to read.
        query_batch(&mut session, &"x".repeat(256 * 1024), None, None)
    };
    if !matches!(result, Err(LeanWorkerError::Timeout { .. })) {
        return Err(format!("send and response did not share deadline: {result:?}"));
    }
    worker
        .health()
        .map_err(|error| format!("replacement is not usable: {error}"))?;
    worker.shutdown().map_err(|error| error.to_string())?;
    Ok(())
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

/// A fake worker whose request timeout is a generous backstop rather than the
/// tight 80ms default. Tests that observe a *child death* (panic/abort, clean
/// exit, crash-on-request) race that backstop against the child's
/// spawn+emit+die+reap sequence; 80ms is enough on a fast host but a loaded CI
/// runner can exceed it, firing a spurious `Timeout` instead of the expected
/// child-death outcome. The hang/timeout conformance tests keep the short
/// default because they assert the timeout fires against a 60s hang, where
/// 80ms is deterministic.
fn fake_worker_observing_child_death(mode: &str) -> Result<LeanWorker, LeanWorkerError> {
    fake_worker_with_config(mode, |config| config.request_timeout(Duration::from_secs(5)))
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

fn conformance_name_rows_complete_with_one_terminal_outcome() -> Result<(), String> {
    let mut worker = fake_worker("normal").map_err(|err| err.to_string())?;
    let generation = worker.lifecycle_snapshot().worker_generation;
    let mut trace = request_trace(generation, "list_declarations_strings");
    let names = list_names(&mut worker, None).map_err(|err| err.to_string())?;
    assert_eq!(names, ["Fake.first", "Fake.second", "Fake.third"]);
    trace.push(RuntimeTraceEvent::TerminalOutcomeObserved {
        generation,
        request: "list_declarations_strings",
        outcome: RuntimeTerminalOutcome::Response("rows_complete"),
    });
    let stats = worker.stats();
    assert_eq!(stats.data_rows_delivered, 3);
    assert_eq!(stats.stream_successes, 1);
    assert_eq!(stats.stream_failures, 0);
    assert_single_terminal(&trace, generation, "list_declarations_strings")?;
    Ok(())
}

fn conformance_name_rows_then_child_exit_fail_the_request() -> Result<(), String> {
    let mut worker = fake_worker_observing_child_death("names_then_exit").map_err(|err| err.to_string())?;
    let error = list_names(&mut worker, None).expect_err("child exits before its terminal response");
    match error {
        LeanWorkerError::ChildExited { exit } => assert!(exit.success),
        other => return Err(format!("expected child exit after one row, got {other:?}")),
    }
    let stats = worker.stats();
    assert_eq!(stats.data_rows_delivered, 1);
    assert_eq!(stats.stream_failures, 1);
    Ok(())
}

fn conformance_name_rows_then_child_abort_fail_the_request() -> Result<(), String> {
    let mut worker = fake_worker_observing_child_death("names_then_abort").map_err(|err| err.to_string())?;
    let error = list_names(&mut worker, None).expect_err("child aborts before its terminal response");
    match error {
        LeanWorkerError::ChildPanicOrAbort { exit } => assert!(!exit.success),
        other => return Err(format!("expected child abort after one row, got {other:?}")),
    }
    let stats = worker.stats();
    assert_eq!(stats.data_rows_delivered, 1);
    assert_eq!(stats.stream_failures, 1);
    Ok(())
}

fn conformance_name_rows_then_silence_time_out() -> Result<(), String> {
    let mut worker = fake_worker("names_then_hang").map_err(|err| err.to_string())?;
    let old_pid = worker.__child_pid_for_test().ok_or("missing worker pid")?;
    let error = list_names(&mut worker, None).expect_err("child falls silent after one row");
    match error {
        LeanWorkerError::Timeout { operation, .. } => assert_eq!(operation, "worker_list_declarations_strings"),
        other => return Err(format!("expected timeout after one row, got {other:?}")),
    }
    assert_reaped(old_pid)?;
    let stats = worker.stats();
    assert_eq!(stats.data_rows_delivered, 1);
    assert_eq!(stats.stream_failures, 1);
    assert_eq!(stats.timeout_restarts, 1);
    Ok(())
}

fn conformance_silent_worker_cancellation_reaps_only_owned_child() -> Result<(), String> {
    struct Started(std::sync::mpsc::Sender<()>);
    impl LeanWorkerProgressSink for Started {
        fn report(&self, _event: LeanWorkerProgressEvent) {
            self.0.send(()).expect("signal first progress tick");
        }
    }
    let mut worker = fake_worker_with_config("progress_then_hang", |config| {
        config.request_timeout(Duration::from_secs(5))
    })
    .map_err(|err| err.to_string())?;
    let mut unrelated = fake_worker("normal").map_err(|err| err.to_string())?;
    let old_pid = worker.__child_pid_for_test().ok_or("missing worker pid")?;
    let unrelated_pid = unrelated.__child_pid_for_test();
    let checkpoint = worker.request_checkpoint();
    let cancellation = LeanWorkerCancellationToken::new();
    let token = cancellation.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let canceller = thread::spawn(move || {
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("request entered worker");
        // Cancellation happens after the last frame, while the child is silent.
        thread::sleep(Duration::from_millis(100));
        token.cancel();
    });
    let started = std::time::Instant::now();
    let error = {
        let mut session = worker
            .open_session(&shims_config(), None, None)
            .map_err(|err| err.to_string())?;
        query_batch(&mut session, "", Some(&cancellation), Some(&Started(sender)))
            .expect_err("silent child must observe cancellation before its request deadline")
    };
    canceller.join().map_err(|_| "canceller thread panicked".to_owned())?;
    assert!(
        matches!(error, LeanWorkerError::Cancelled { .. }),
        "unexpected outcome: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "silent cancellation waited for request timeout"
    );
    assert_reaped(old_pid)?;
    let receipt = worker
        .confirmed_request_stop_since(&checkpoint)
        .ok_or("cancellation lost direct-worker exit")?;
    assert_eq!(receipt.process_id(), old_pid);
    assert_eq!(receipt.operation(), "worker_process_module_query_batch");
    assert_ne!(worker.__child_pid_for_test(), Some(old_pid));
    assert_eq!(worker.stats().cancelled_restarts, 1);
    worker.health().map_err(|err| err.to_string())?;
    unrelated.health().map_err(|err| err.to_string())?;
    assert_eq!(unrelated.__child_pid_for_test(), unrelated_pid);
    Ok(())
}

fn conformance_silent_worker_deadline_is_not_renewed_by_polling() -> Result<(), String> {
    for token_present in [false, true] {
        let timeout = Duration::from_millis(180);
        let mut worker = fake_worker_with_config("query_hang", |config| config.request_timeout(timeout))
            .map_err(|err| err.to_string())?;
        let old_pid = worker.__child_pid_for_test().ok_or("missing worker pid")?;
        let token = LeanWorkerCancellationToken::new();
        let (started, error) = {
            let mut session = worker
                .open_session(&shims_config(), None, None)
                .map_err(|err| err.to_string())?;
            let started = std::time::Instant::now();
            let error = query_batch(&mut session, "", token_present.then_some(&token), None)
                .expect_err("silent child must reach original request deadline");
            (started, error)
        };
        assert!(matches!(error, LeanWorkerError::Timeout { duration, .. } if duration == timeout));
        assert!(started.elapsed() >= timeout);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "polling renewed the original deadline"
        );
        assert_reaped(old_pid)?;
        assert_eq!(worker.stats().timeout_restarts, 1);
        assert_eq!(worker.stats().cancelled_restarts, 0);
    }
    Ok(())
}

fn conformance_slow_progress_sink_backpressure_is_bounded_and_observable() -> Result<(), String> {
    struct SlowSink(AtomicU64);
    impl LeanWorkerProgressSink for SlowSink {
        fn report(&self, _event: LeanWorkerProgressEvent) {
            thread::sleep(Duration::from_millis(1));
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    // Hosted macOS can coalesce each one-millisecond sleep heavily. Keep a
    // finite, generous backstop rather than read timer scheduling as a
    // protocol failure; the timeout tests keep their short deadlines.
    let mut worker = fake_worker_with_config("many_progress", |config| {
        config.request_timeout(Duration::from_secs(120))
    })
    .map_err(|err| err.to_string())?;
    let sink = SlowSink(AtomicU64::new(0));
    let outcome = {
        let mut session = worker
            .open_session(&shims_config(), None, None)
            .map_err(|err| err.to_string())?;
        query_batch(&mut session, "", None, Some(&sink)).map_err(|err| err.to_string())?
    };
    assert!(matches!(outcome, LeanWorkerModuleQueryBatchOutcome::Ok { .. }));
    assert_eq!(sink.0.load(Ordering::Relaxed), 256);
    assert!(
        worker.stats().backpressure_waits > 0,
        "a slow sink should fill the reader's bounded buffer"
    );
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

fn request_stop_receipt_matches_checkpoint_across_replacement() -> Result<(), String> {
    let mut worker = fake_worker("request_hang").map_err(|err| err.to_string())?;
    let other = fake_worker("normal").map_err(|err| err.to_string())?;
    let checkpoint = worker.request_checkpoint();
    let other_checkpoint = other.request_checkpoint();
    let generation = worker.lifecycle_snapshot().worker_generation;
    let old_pid = worker.__child_pid_for_test().ok_or("missing worker child")?;
    assert!(worker.confirmed_request_stop_since(&checkpoint).is_none());
    assert!(matches!(worker.health(), Err(LeanWorkerError::Timeout { .. })));
    let receipt = worker
        .confirmed_request_stop_since(&checkpoint)
        .ok_or("timeout lost its confirmed direct-worker exit")?;
    assert_eq!(receipt.worker_generation(), generation);
    assert_eq!(receipt.process_id(), old_pid);
    assert_eq!(receipt.operation(), "health");
    assert!(!receipt.exit().success);
    assert_reaped(old_pid)?;
    assert_eq!(
        worker.lifecycle_snapshot().worker_generation,
        generation.saturating_add(1)
    );
    assert!(worker.lifecycle_snapshot().last_exit.is_none(), "replacement is live");
    assert!(worker.confirmed_request_stop_since(&other_checkpoint).is_none());
    let stopped_request_id = receipt.request_id();
    let next_checkpoint = worker.request_checkpoint();
    assert!(
        worker
            .confirmed_request_stop_since(&worker.request_checkpoint())
            .is_none()
    );
    assert!(matches!(worker.health(), Err(LeanWorkerError::Timeout { .. })));
    let next = worker
        .confirmed_request_stop_since(&next_checkpoint)
        .ok_or("second timeout lost exit")?;
    assert_eq!(next.request_id(), stopped_request_id.saturating_add(1));
    assert_eq!(next.worker_generation(), generation.saturating_add(1));
    assert_ne!(next.process_id(), old_pid);
    Ok(())
}

fn newer_request_shadows_older_stop_receipt() -> Result<(), String> {
    let mut worker = fake_worker_with_config("query_hang", |config| {
        config.request_timeout(Duration::from_millis(300))
    })
    .map_err(|error| error.to_string())?;
    let checkpoint = worker.request_checkpoint();
    let result = {
        let mut session = worker
            .open_session(&shims_config(), None, None)
            .map_err(|error| error.to_string())?;
        query_batch(&mut session, &"x".repeat(256 * 1024), None, None)
    };
    assert!(matches!(result, Err(LeanWorkerError::Timeout { .. })));
    assert!(worker.confirmed_request_stop_since(&checkpoint).is_some());
    worker.health().map_err(|error| error.to_string())?;
    assert!(
        worker.confirmed_request_stop_since(&checkpoint).is_none(),
        "a newer live generation's request cannot use the old exit receipt"
    );
    let idle_checkpoint = worker.request_checkpoint();
    worker.cycle().map_err(|error| error.to_string())?;
    assert!(
        worker.confirmed_request_stop_since(&idle_checkpoint).is_none(),
        "an idle cycle must not invent a post-checkpoint request"
    );
    Ok(())
}

fn conformance_child_crash_terminalizes_in_flight_request() -> Result<(), String> {
    let mut worker = fake_worker_observing_child_death("crash_on_health").map_err(|err| err.to_string())?;
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

/// The residue bound end to end against a child that reports region bytes:
/// the running sum is what the parent acts on, and it is a *generation*
/// quantity, so the replacement child starts from zero even though
/// `LeanWorkerStats` is carried forward across the restart.
fn import_residue_accumulates_over_a_generation_and_resets_on_the_cycle() -> Result<(), String> {
    let project = FakeProject::new("import-residue")?;
    // Three imports' worth. The check runs *before* a request, so the cycle
    // lands on the open after the running sum reaches the limit, never on the
    // import that carried it there.
    let limit = FAKE_IMPORT_RESIDUE_BYTES * 3;
    let mut handle = project
        .builder("residue_reporting", ["Init"])?
        .restart_policy(LeanWorkerRestartPolicy::default().max_import_residue_bytes(limit))
        .open()
        .map_err(|err| err.to_string())?;

    // Opening the handle already imported once.
    let opened = handle.lifecycle_snapshot();
    assert_eq!(opened.import_residue_bytes, FAKE_IMPORT_RESIDUE_BYTES);
    assert_eq!(opened.import_residue_limit_bytes, Some(limit));
    assert_eq!(opened.worker_generation, 0);

    for import in ["A", "B"] {
        let _session = handle
            .open_session_with_imports([import], None, None)
            .map_err(|err| err.to_string())?;
    }
    let charged = handle.lifecycle_snapshot();
    assert_eq!(charged.import_residue_bytes, limit);
    assert_eq!(charged.worker_generation, 0, "reaching the limit is not itself a cycle");
    assert_eq!(handle.stats().import_residue_restarts, 0);

    // The next open is admitted only after the child is replaced, and the import
    // it then performs is the new generation's first and only charge.
    let _session = handle
        .open_session_with_imports(["C"], None, None)
        .map_err(|err| err.to_string())?;
    let cycled = handle.lifecycle_snapshot();
    assert_eq!(cycled.worker_generation, 1);
    assert_eq!(
        cycled.import_residue_bytes, FAKE_IMPORT_RESIDUE_BYTES,
        "the budget is per generation, even though lifetime stats carry forward"
    );
    let stats = handle.stats();
    assert_eq!(stats.import_residue_restarts, 1);
    assert_eq!(stats.imports, 4, "the lifetime import count spans both generations");
    assert_eq!(
        stats.import_stats_unusable, 0,
        "this child attributes region bytes, so nothing is unusable"
    );
    assert_eq!(
        cycled
            .last_restart_reason
            .as_ref()
            .map(LeanWorkerRestartReason::stable_cause),
        Some("import_residue")
    );
    Ok(())
}

/// The degradation this design has to survive: a child whose shim reports
/// modules but no region bytes. The byte bound can never fire, so it must be
/// visible in the stats and the count backstop must still bound the child.
fn a_child_that_reports_no_region_bytes_is_counted_and_still_bounded() -> Result<(), String> {
    let project = FakeProject::new("import-residue-unusable")?;
    let mut handle = project
        .builder("normal", ["Init"])?
        .restart_policy(
            LeanWorkerRestartPolicy::default()
                .max_import_residue_bytes(FAKE_IMPORT_RESIDUE_BYTES)
                .max_imports(2),
        )
        .open()
        .map_err(|err| err.to_string())?;

    for import in ["A", "B", "C"] {
        let _session = handle
            .open_session_with_imports([import], None, None)
            .map_err(|err| err.to_string())?;
    }
    let snapshot = handle.lifecycle_snapshot();
    assert_eq!(snapshot.import_residue_bytes, 0, "nothing to accumulate");
    assert_eq!(snapshot.worker_generation, 1, "the count backstop cycled the child");
    assert_eq!(
        snapshot
            .last_restart_reason
            .as_ref()
            .map(LeanWorkerRestartReason::stable_cause),
        Some("max_imports")
    );
    let stats = handle.stats();
    assert_eq!(stats.import_residue_restarts, 0);
    assert_eq!(
        stats.import_stats_unusable, 4,
        "every import — the handle's own plus the three here — reported modules with no bytes attributed to them"
    );
    Ok(())
}

fn stale_warm_session_keeps_import_count_backstop() -> Result<(), String> {
    let project = FakeProject::new("session-count")?;
    let mut handle = project
        .builder("session_count", ["Init"])?
        .request_timeout(Duration::from_secs(5))
        .restart_policy(LeanWorkerRestartPolicy::default().max_imports(1))
        .open()
        .map_err(|err| err.to_string())?;
    {
        let _session = handle.open_session(None, None).map_err(|err| err.to_string())?;
    };
    assert_eq!(handle.stats().restarts, 0, "warm reuse above count cap");
    {
        let _session = handle.open_session(None, None).map_err(|err| err.to_string())?;
    };
    assert_eq!(
        handle.stats().max_import_restarts,
        1,
        "actual stale miss still enforces count"
    );
    Ok(())
}

fn failed_header_then_name_listing_reopens_session() -> Result<(), String> {
    let project = FakeProject::new("header-name-listing")?;
    let mut handle = project
        .builder("header_failure", ["Init"])?
        .request_timeout(Duration::from_secs(5))
        .open()
        .map_err(|err| err.to_string())?;
    {
        let mut session = handle.open_session(None, None).map_err(|err| err.to_string())?;
        for _ in 0..2 {
            let error = query_batch(&mut session, "bad", None, None).expect_err("invalid source");
            assert!(error.to_string().contains("original bad import diagnostic"));
            session
                .list_declarations_strings(&LeanWorkerDeclarationFilter::default(), None, None)
                .map_err(|err| err.to_string())?;
        }
    }
    assert_eq!(handle.stats().restarts, 2);
    Ok(())
}

fn actual_session_miss_recycles_once_but_warm_reuse_does_not() -> Result<(), String> {
    let project = FakeProject::new("session-budget")?;
    let mut handle = project
        .builder("session_budget", ["Init"])?
        .request_timeout(Duration::from_secs(5))
        .restart_policy(LeanWorkerRestartPolicy::default().max_import_residue_bytes(1000))
        .open()
        .map_err(|err| err.to_string())?;
    for import in ["Init", "Init", "Other", "Other"] {
        {
            let _session = handle
                .open_session_with_imports([import], None, None)
                .map_err(|err| err.to_string())?;
        };
    }
    assert_eq!(handle.stats().restarts, 1);
    Ok(())
}

fn exercise_header_sources(mode: &str, sources: &[&str]) -> Result<u64, String> {
    let project = FakeProject::new("header-memory")?;
    let mut handle = project
        .builder(mode, ["Init"])?
        .request_timeout(Duration::from_secs(5))
        .restart_policy(LeanWorkerRestartPolicy::default().max_import_residue_bytes(1000))
        .open()
        .map_err(|err| err.to_string())?;
    {
        let mut session = handle.open_session(None, None).map_err(|err| err.to_string())?;
        for source in sources {
            let result = session.process_module_query_batch(
                source,
                &[],
                &LeanWorkerOutputBudgets::default(),
                &LeanWorkerElabOptions::default(),
                None,
                None,
            );
            if *source == "bad" {
                let error = result.expect_err("invalid import must preserve failure");
                assert!(error.to_string().contains("original bad import diagnostic"), "{error}");
            } else if mode == "header_always_recycle" {
                assert!(result.is_err(), "second recycle response must not become a pass");
            } else {
                assert!(
                    matches!(result, Ok(LeanWorkerModuleQueryBatchOutcome::Ok { .. })),
                    "original result preserved: {result:?}"
                );
            }
        }
    }
    Ok(handle.stats().restarts)
}

fn header_budget_reuses_warm_and_retries_distinct_once() -> Result<(), String> {
    assert_eq!(exercise_header_sources("header_budget", &["A", "A"])?, 0);
    assert_eq!(exercise_header_sources("header_budget", &["A", "A", "B", "B"])?, 1);
    Ok(())
}

fn failed_header_preserves_result_and_retires_next_request() -> Result<(), String> {
    assert_eq!(exercise_header_sources("header_failure", &["bad"])?, 0);
    assert_eq!(exercise_header_sources("header_failure", &["bad", "bad"])?, 1);
    Ok(())
}

fn header_recycle_is_retried_only_once() -> Result<(), String> {
    assert_eq!(exercise_header_sources("header_always_recycle", &["A"])?, 1);
    Ok(())
}

fn declaration_outline_batch_selector_reaches_parent_session_path() -> Result<(), String> {
    let project = FakeProject::new("declaration-outline")?;
    let mut handle = project
        .builder("normal", ["Init"])?
        .open()
        .map_err(|err| err.to_string())?;
    let mut session = handle.open_session(None, None).map_err(|err| err.to_string())?;

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
    let project = FakeProject::new("declaration-verification-batch")?;
    let mut handle = project
        .builder("normal", ["Init"])?
        .open()
        .map_err(|err| err.to_string())?;
    let mut session = handle.open_session(None, None).map_err(|err| err.to_string())?;

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
    let project = FakeProject::new("command-message")?;
    let mut handle = project
        .builder("normal", ["Init"])?
        .open()
        .map_err(|err| err.to_string())?;
    let mut session = handle.open_session(None, None).map_err(|err| err.to_string())?;

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

/// A scratch project root for tests that open sessions through a host handle.
///
/// The root holds the wrapper scripts that start this test binary as a fake
/// child in a chosen mode, and it is the `LEAN_SYSROOT` that child sees.
struct FakeProject {
    root: PathBuf,
}

impl FakeProject {
    fn new(name: &str) -> Result<Self, String> {
        let root = env::temp_dir().join(format!(
            "lean-rs-worker-parent-{name}-{}-{}",
            std::process::id(),
            thread_id_suffix()
        ));
        std::fs::create_dir_all(&root).map_err(|err| format!("create fake project root: {err}"))?;
        Ok(Self { root })
    }

    fn builder<const N: usize>(&self, mode: &str, imports: [&str; N]) -> Result<LeanWorkerHostHandleBuilder, String> {
        let current_exe = env::current_exe().map_err(|err| format!("resolve current test executable: {err}"))?;
        let wrapper = self.root.join(wrapper_name(mode));
        write_fake_child_wrapper(&wrapper, &current_exe, mode)?;
        Ok(LeanWorkerHostHandleBuilder::shims_only(&self.root, imports)
            .worker_child(LeanWorkerChild::for_toolchain(wrapper, &self.root))
            .startup_timeout(Duration::from_secs(1))
            .request_timeout(Duration::from_millis(80))
            .shutdown_timeout(Duration::from_millis(80)))
    }
}

impl Drop for FakeProject {
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

#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "query the exact Darwin process status without spawning ps")]
fn process_state(pid: u32) -> Result<ProcessState, String> {
    let native = i32::try_from(pid).map_err(|err| err.to_string())?;
    if native <= 0 {
        return Err(format!("invalid process PID {pid}"));
    }
    // SAFETY: zeroed proc_bsdinfo is valid writable C output storage.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of_val(&info)).map_err(|err| err.to_string())?;
    // SAFETY: errno is thread-local, and info is writable for exactly size bytes.
    let got = unsafe {
        *libc::__error() = 0;
        libc::proc_pidinfo(native, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
    };
    let error = io::Error::last_os_error();
    if got == 0 && error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(ProcessState::Missing);
    }
    if got != size || info.pbi_pid != pid {
        return Err(format!(
            "incomplete process status for PID {pid} ({got}/{size} bytes): {error}"
        ));
    }
    Ok(if info.pbi_status == libc::SZOMB {
        ProcessState::Zombie
    } else {
        ProcessState::Alive
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
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

fn public_worker_handles_keep_unwind_safety() {
    fn assert_unwind_safe<T: std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
    assert_unwind_safe::<LeanWorker>();
    assert_unwind_safe::<lean_rs_worker_parent::LeanWorkerHostHandle>();
}

fn expected_host_runtime() -> LeanWorkerHostRuntimeExpectation {
    LeanWorkerHostRuntimeExpectation {
        compiled_header_sha256: Some("b".repeat(64)),
        runtime_lean_version: Some("4.35.0".into()),
        checked_host_bindings: std::collections::BTreeMap::from([("verify_declaration_batch".into(), "a".repeat(64))]),
    }
}
fn host_metadata_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::shims_only("/fake-metadata-project", ["Init"])
        .with_host_runtime_expectation(expected_host_runtime())
}
fn host_metadata_is_checked_before_session_is_exposed() -> Result<(), String> {
    let mut worker = fake_worker("host_metadata_bad").map_err(|e| e.to_string())?;
    let checkpoint = worker.request_checkpoint();
    let result = worker.open_session(&host_metadata_config(), None, None);
    assert!(result.is_err());
    drop(result);
    assert!(worker.host_runtime_metadata().is_none());
    assert_eq!(
        worker.host_session_refusal_since(&checkpoint),
        Some("worker_host_runtime_metadata")
    );
    assert!(
        worker.confirmed_request_stop_since(&checkpoint).is_none(),
        "metadata refusal is not an exit receipt"
    );
    worker.try_shutdown().map_err(|e| e.to_string())?;
    Ok(())
}
fn host_metadata_is_revalidated_after_recycle() -> Result<(), String> {
    let mut worker = fake_worker("host_metadata_ok").map_err(|e| e.to_string())?;
    {
        let _session = worker
            .open_session(&host_metadata_config(), None, None)
            .map_err(|e| e.to_string())?;
    }
    assert!(worker.host_runtime_metadata().is_some());
    worker.cycle().map_err(|e| e.to_string())?;
    assert!(worker.host_runtime_metadata().is_none());
    let mut wrong = host_metadata_config();
    wrong = wrong.with_host_runtime_expectation(LeanWorkerHostRuntimeExpectation {
        runtime_lean_version: Some("different".into()),
        ..Default::default()
    });
    let result = worker.open_session(&wrong, None, None);
    assert!(result.is_err());
    drop(result);
    assert!(worker.host_runtime_metadata().is_none());
    worker.try_shutdown().map_err(|e| e.to_string())?;
    Ok(())
}
fn host_handle_requires_host_metadata_before_first_session() -> Result<(), String> {
    let project = FakeProject::new("handle-first-host-metadata")?;
    let result = project
        .builder("host_metadata_bad", ["Init"])?
        .require_host_runtime(expected_host_runtime())
        .open();
    let error = result.err().ok_or("mismatched host runtime exposed a host handle")?;
    assert!(error.to_string().contains("host runtime compatibility"), "{error}");
    Ok(())
}

fn host_handle_revalidates_host_metadata_after_replacement() -> Result<(), String> {
    let project = FakeProject::new("handle-replacement-host-metadata")?;
    let mut handle = project
        .builder("host_metadata_ok", ["Init"])?
        .require_host_runtime(expected_host_runtime())
        .open()
        .map_err(|error| error.to_string())?;
    let wrapper = project.root.join(wrapper_name("host_metadata_ok"));
    write_fake_child_wrapper(
        &wrapper,
        &env::current_exe().map_err(|error| error.to_string())?,
        "host_metadata_bad",
    )?;
    handle.cycle().map_err(|error| error.to_string())?;
    let checkpoint = handle.request_checkpoint();
    let result = handle
        .open_session_with_imports(["Std"], None, None)
        .and_then(|mut session| {
            session.capability_metadata("fake_capability_metadata", &serde_json::json!({}), None, None)
        });
    let error = result.err().ok_or("replacement bypassed required host runtime")?;
    assert!(error.to_string().contains("host runtime compatibility"), "{error}");
    assert!(
        !project.root.join("capability-export-dispatched").exists(),
        "no export after replacement ABI refusal"
    );
    assert_eq!(
        handle.host_session_refusal_since(&checkpoint),
        Some("worker_host_runtime_metadata")
    );
    assert!(
        handle.confirmed_request_stop_since(&checkpoint).is_none(),
        "pre-body refusal does not claim process exit"
    );
    handle.try_shutdown().map_err(|error| error.to_string())?;
    Ok(())
}

fn host_metadata_unobserved_protocol_failure_has_no_terminal_refusal() -> Result<(), String> {
    let mut worker = fake_worker("host_metadata_wrong_response").map_err(|error| error.to_string())?;
    let checkpoint = worker.request_checkpoint();
    let result = worker.open_session(&host_metadata_config(), None, None);
    let error = result.err().ok_or("wrong metadata frame exposed a host session")?;
    assert!(matches!(error, LeanWorkerError::Protocol { .. }));
    assert!(
        worker.host_session_refusal_since(&checkpoint).is_none(),
        "no typed metadata result was observed"
    );
    assert!(worker.host_runtime_metadata().is_none());
    worker.try_shutdown().map_err(|error| error.to_string())?;
    Ok(())
}

fn host_metadata_accepts_compatible_extra_bindings() -> Result<(), String> {
    let mut worker = fake_worker("host_metadata_ok").map_err(|e| e.to_string())?;
    {
        let _session = worker
            .open_session(&host_metadata_config(), None, None)
            .map_err(|e| e.to_string())?;
    }
    assert!(
        worker
            .host_runtime_metadata()
            .is_some_and(|m| m.checked_host_bindings.contains_key("new_optional_binding"))
    );
    worker.try_shutdown().map_err(|e| e.to_string())?;
    Ok(())
}

fn artifact_epoch_change_recycles_all_pooled_state_before_body() -> Result<(), String> {
    let mut worker = fake_worker("artifact_epoch").map_err(|error| error.to_string())?;
    {
        let _session = worker
            .open_session(&host_metadata_config(), None, None)
            .map_err(|error| error.to_string())?;
    }
    let first = worker.host_runtime_metadata().cloned();
    {
        let _session = worker
            .open_session(&host_metadata_config(), None, None)
            .map_err(|error| error.to_string())?;
    }
    assert_eq!(
        worker.stats().restarts,
        1,
        "changed output forces a complete process cycle"
    );
    assert_eq!(
        worker.stats().imports,
        2,
        "new process imports rather than reusing old pooled state"
    );
    assert_eq!(worker.host_runtime_metadata(), first.as_ref());
    worker.try_shutdown().map_err(|error| error.to_string())?;
    Ok(())
}

fn terminal_refusal_survives_until_later_dispatch() -> Result<(), String> {
    let mut worker = fake_worker("refuse_once").map_err(|error| error.to_string())?;
    let checkpoint = worker.request_checkpoint();
    let error = worker
        .open_session(&host_metadata_config(), None, None)
        .err()
        .ok_or_else(|| "fixture must refuse its first host open".to_owned())?;
    assert!(matches!(error, LeanWorkerError::Worker { ref code, .. } if code == "fake.import_refused"));
    assert_eq!(
        worker.host_session_refusal_since(&checkpoint),
        Some("open_worker_session")
    );
    assert!(
        worker
            .host_session_refusal_since(&worker.request_checkpoint())
            .is_none(),
        "a newer checkpoint cannot reuse it"
    );
    let mut foreign = fake_worker("normal").map_err(|error| error.to_string())?;
    assert!(
        worker
            .host_session_refusal_since(&foreign.request_checkpoint())
            .is_none(),
        "another supervisor cannot reuse it"
    );
    foreign.try_shutdown().map_err(|error| error.to_string())?;
    worker.health().map_err(|error| error.to_string())?;
    assert!(
        worker.host_session_refusal_since(&checkpoint).is_none(),
        "later typed dispatch clears stale target phase"
    );
    {
        let mut session = worker
            .open_session(&host_metadata_config(), None, None)
            .map_err(|error| error.to_string())?;
        let outcome = session
            .process_module_query_batch(
                "theorem fixture : True := True.intro",
                &[LeanWorkerModuleQuerySelector::Diagnostics {
                    id: "later-body".to_owned(),
                }],
                &LeanWorkerOutputBudgets::default(),
                &LeanWorkerElabOptions::default(),
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        assert!(matches!(outcome, LeanWorkerModuleQueryBatchOutcome::Ok { .. }));
    }
    assert!(
        worker.host_session_refusal_since(&checkpoint).is_none(),
        "successful session/body in the same checkpoint has no earlier refusal"
    );
    worker.try_shutdown().map_err(|error| error.to_string())?;
    Ok(())
}
