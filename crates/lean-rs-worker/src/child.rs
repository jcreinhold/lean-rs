use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lean_rs::{LeanError, LeanResult, LeanRuntime};
use lean_rs_host::LeanHost;

use crate::protocol::{Message, Request, Response, read_frame, write_frame};

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

#[allow(dead_code, reason = "reserved for prompt 57 worker configuration")]
fn _path_for_diagnostics(path: &Path) -> PathBuf {
    path.to_path_buf()
}
