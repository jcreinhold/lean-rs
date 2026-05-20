use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lean_rs::{LeanError, LeanResult, LeanRuntime};
use lean_rs_host::{
    LeanCapabilities, LeanElabFailure, LeanElabOptions, LeanHost, LeanKernelOutcome, LeanSession, LeanSeverity,
};

use crate::protocol::{
    Message, ProgressTick, Request, Response, WorkerDiagnostic, WorkerElabOptions, WorkerElabOutcome,
    WorkerKernelOutcome, WorkerKernelStatus, read_frame, write_frame,
};

pub(crate) fn run_stdio() -> ExitCode {
    match serve_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("lean-rs-worker-child: {err}");
            ExitCode::FAILURE
        }
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the child owns stdin/stdout for the full protocol loop"
)]
fn serve_stdio() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = LeanRuntime::init()?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut host_session: Option<HostSessionState> = None;

    write_frame(
        &mut writer,
        Message::Handshake {
            worker_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_version: crate::protocol::PROTOCOL_VERSION,
        },
    )?;

    loop {
        let frame = read_frame(&mut reader)?;
        let Message::Request(request) = frame.message else {
            write_frame(
                &mut writer,
                Message::Response(Response::Error {
                    code: "lean_rs.worker.protocol.unexpected_frame".to_owned(),
                    message: "child expected request frame".to_owned(),
                }),
            )?;
            continue;
        };

        match request {
            Request::Health => {
                write_frame(&mut writer, Message::Response(Response::HealthOk))?;
            }
            Request::LoadFixtureCapability { fixture_root } => {
                let response = match load_fixture_capability(runtime, Path::new(&fixture_root)) {
                    Ok(()) => Response::CapabilityLoaded,
                    Err(err) => error_response(&err),
                };
                write_frame(&mut writer, Message::Response(response))?;
            }
            Request::CallFixtureMul { fixture_root, lhs, rhs } => {
                let response = match call_fixture_mul(runtime, Path::new(&fixture_root), lhs, rhs) {
                    Ok(value) => Response::U64 { value },
                    Err(err) => error_response(&err),
                };
                write_frame(&mut writer, Message::Response(response))?;
            }
            Request::TriggerLeanPanic { fixture_root } => {
                let response = match trigger_lean_panic(runtime, Path::new(&fixture_root)) {
                    Ok(()) => Response::Error {
                        code: "lean_rs.worker.panic_fixture_returned".to_owned(),
                        message: "Lean panic fixture returned instead of terminating the child".to_owned(),
                    },
                    Err(err) => error_response(&err),
                };
                write_frame(&mut writer, Message::Response(response))?;
            }
            Request::OpenHostSession {
                project_root,
                package,
                lib_name,
                imports,
            } => {
                let response = match HostSessionState::open(runtime, &project_root, &package, &lib_name, &imports) {
                    Ok(state) => {
                        host_session = Some(state);
                        Response::HostSessionOpened
                    }
                    Err(err) => error_response(&err),
                };
                write_frame(&mut writer, Message::Response(response))?;
            }
            Request::Elaborate { source, options } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.elaborate(&source, &options) {
                        Ok(outcome) => Response::Elaboration { outcome },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_frame(&mut writer, Message::Response(response))?;
            }
            Request::KernelCheck {
                source,
                options,
                progress,
            } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.kernel_check(&source, &options, progress, &mut writer) {
                        Ok(outcome) => Response::KernelCheck { outcome },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_frame(&mut writer, Message::Response(response))?;
            }
            Request::DeclarationKinds { names, progress } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.declaration_kinds(&names, progress, &mut writer) {
                        Ok(values) => Response::Strings { values },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_frame(&mut writer, Message::Response(response))?;
            }
            Request::DeclarationNames { names, progress } => {
                let response = match host_session.as_mut() {
                    Some(state) => match state.declaration_names(&names, progress, &mut writer) {
                        Ok(values) => Response::Strings { values },
                        Err(err) => error_response(&err),
                    },
                    None => missing_session_response(),
                };
                write_frame(&mut writer, Message::Response(response))?;
            }
            Request::Terminate => {
                write_frame(&mut writer, Message::Response(Response::Terminating))?;
                return Ok(());
            }
        }
    }
}

fn load_fixture_capability(runtime: &'static LeanRuntime, fixture_root: &Path) -> LeanResult<()> {
    let host = LeanHost::from_lake_project(runtime, fixture_root)?;
    let _caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;
    Ok(())
}

fn call_fixture_mul(runtime: &'static LeanRuntime, fixture_root: &Path, lhs: u64, rhs: u64) -> LeanResult<u64> {
    let host = LeanHost::from_lake_project(runtime, fixture_root)?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;
    let mut session = caps.session(&["LeanRsFixture.Scalars"], None, None)?;
    session.call_capability::<(u64, u64), u64>("lean_rs_fixture_u64_mul", (lhs, rhs), None)
}

fn trigger_lean_panic(runtime: &'static LeanRuntime, fixture_root: &Path) -> LeanResult<()> {
    let host = LeanHost::from_lake_project(runtime, fixture_root)?;
    let caps = host.load_capabilities("lean_rs_fixture", "LeanRsFixture")?;
    let mut session = caps.session(&["LeanRsFixture.Effects"], None, None)?;
    session.call_capability::<(u8,), ()>("lean_rs_fixture_panic_unit", (0,), None)
}

fn error_response(err: &LeanError) -> Response {
    Response::Error {
        code: err.code().as_str().to_owned(),
        message: err.to_string(),
    }
}

fn missing_session_response() -> Response {
    Response::Error {
        code: "lean_rs.worker.session_missing".to_owned(),
        message: "open a LeanWorkerSession before sending host-session requests".to_owned(),
    }
}

struct HostSessionState {
    #[allow(dead_code, reason = "leaked host anchors the capability and session lifetimes")]
    host: &'static LeanHost<'static>,
    #[allow(dead_code, reason = "leaked capabilities anchor the session borrow")]
    capabilities: &'static LeanCapabilities<'static, 'static>,
    session: LeanSession<'static, 'static>,
}

impl HostSessionState {
    fn open(
        runtime: &'static LeanRuntime,
        project_root: &str,
        package: &str,
        lib_name: &str,
        imports: &[String],
    ) -> LeanResult<Self> {
        let host = Box::leak(Box::new(LeanHost::from_lake_project(runtime, Path::new(project_root))?));
        let capabilities = Box::leak(Box::new(host.load_capabilities(package, lib_name)?));
        let import_refs: Vec<&str> = imports.iter().map(String::as_str).collect();
        let session = capabilities.session(&import_refs, None, None)?;
        Ok(Self {
            host,
            capabilities,
            session,
        })
    }

    fn elaborate(&mut self, source: &str, options: &WorkerElabOptions) -> LeanResult<WorkerElabOutcome> {
        let options = options.to_host_options();
        let outcome = self.session.elaborate(source, None, &options, None)?;
        Ok(match outcome {
            Ok(_expr) => WorkerElabOutcome {
                success: true,
                diagnostics: Vec::new(),
                truncated: false,
            },
            Err(failure) => elab_failure_outcome(&failure),
        })
    }

    fn kernel_check(
        &mut self,
        source: &str,
        options: &WorkerElabOptions,
        progress: bool,
        writer: &mut impl std::io::Write,
    ) -> LeanResult<WorkerKernelOutcome> {
        if progress {
            emit_progress(writer, "kernel_check", 0, Some(1));
        }
        let options = options.to_host_options();
        let outcome = self.session.kernel_check(source, &options, None, None)?;
        if progress {
            emit_progress(writer, "kernel_check", 1, Some(1));
        }
        Ok(match outcome {
            LeanKernelOutcome::Checked(_) => WorkerKernelOutcome {
                status: WorkerKernelStatus::Checked,
                diagnostics: Vec::new(),
                truncated: false,
            },
            LeanKernelOutcome::Rejected(failure) => kernel_failure_outcome(WorkerKernelStatus::Rejected, &failure),
            LeanKernelOutcome::Unavailable(failure) => {
                kernel_failure_outcome(WorkerKernelStatus::Unavailable, &failure)
            }
            LeanKernelOutcome::Unsupported(failure) => {
                kernel_failure_outcome(WorkerKernelStatus::Unsupported, &failure)
            }
            _ => WorkerKernelOutcome {
                status: WorkerKernelStatus::Unsupported,
                diagnostics: Vec::new(),
                truncated: false,
            },
        })
    }

    fn declaration_kinds(
        &mut self,
        names: &[String],
        progress: bool,
        writer: &mut impl std::io::Write,
    ) -> LeanResult<Vec<String>> {
        if progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let mut out = Vec::with_capacity(names.len());
            for (idx, name) in names.iter().enumerate() {
                out.push(self.session.declaration_kind(name, None)?);
                emit_progress(
                    writer,
                    "declaration_kind_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                );
            }
            Ok(out)
        } else {
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.session.declaration_kind_bulk(&refs, None, None)
        }
    }

    fn declaration_names(
        &mut self,
        names: &[String],
        progress: bool,
        writer: &mut impl std::io::Write,
    ) -> LeanResult<Vec<String>> {
        if progress {
            let total = Some(u64::try_from(names.len()).unwrap_or(u64::MAX));
            let mut out = Vec::with_capacity(names.len());
            for (idx, name) in names.iter().enumerate() {
                out.push(self.session.declaration_name(name, None)?);
                emit_progress(
                    writer,
                    "declaration_name_bulk",
                    u64::try_from(idx.saturating_add(1)).unwrap_or(u64::MAX),
                    total,
                );
            }
            Ok(out)
        } else {
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.session.declaration_name_bulk(&refs, None, None)
        }
    }
}

impl WorkerElabOptions {
    fn to_host_options(&self) -> LeanElabOptions {
        LeanElabOptions::new()
            .namespace_context(&self.namespace_context)
            .file_label(&self.file_label)
            .heartbeat_limit(self.heartbeat_limit)
            .diagnostic_byte_limit(self.diagnostic_byte_limit)
    }
}

fn elab_failure_outcome(failure: &LeanElabFailure) -> WorkerElabOutcome {
    WorkerElabOutcome {
        success: false,
        diagnostics: diagnostics(failure),
        truncated: failure.truncated(),
    }
}

fn kernel_failure_outcome(status: WorkerKernelStatus, failure: &LeanElabFailure) -> WorkerKernelOutcome {
    WorkerKernelOutcome {
        status,
        diagnostics: diagnostics(failure),
        truncated: failure.truncated(),
    }
}

fn diagnostics(failure: &LeanElabFailure) -> Vec<WorkerDiagnostic> {
    failure
        .diagnostics()
        .iter()
        .map(|diagnostic| {
            let (line, column, end_line, end_column) =
                diagnostic.position().map_or((None, None, None, None), |position| {
                    (
                        Some(position.line()),
                        Some(position.column()),
                        position.end_line(),
                        position.end_column(),
                    )
                });
            WorkerDiagnostic {
                severity: match diagnostic.severity() {
                    LeanSeverity::Info => "info",
                    LeanSeverity::Warning => "warning",
                    LeanSeverity::Error => "error",
                    _ => "unknown",
                }
                .to_owned(),
                message: diagnostic.message().to_owned(),
                file_label: diagnostic.file_label().to_owned(),
                line,
                column,
                end_line,
                end_column,
            }
        })
        .collect()
}

fn emit_progress(writer: &mut impl std::io::Write, phase: &str, current: u64, total: Option<u64>) {
    drop(write_frame(
        writer,
        Message::ProgressTick(ProgressTick {
            phase: phase.to_owned(),
            current,
            total,
        }),
    ));
}

#[allow(dead_code, reason = "reserved for prompt 57 worker configuration")]
fn _path_for_diagnostics(path: &Path) -> PathBuf {
    path.to_path_buf()
}
