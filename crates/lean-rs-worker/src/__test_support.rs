//! Hidden harness for prompt-56 integration tests.
//!
//! This is not a supervisor API. It exists so tests can drive the private
//! worker protocol without exposing protocol frames as the crate's public
//! abstraction.

#![allow(
    clippy::wildcard_enum_match_arm,
    reason = "hidden test harness reports any unexpected protocol message with Debug"
)]

use std::fmt;
use std::io::{BufReader, BufWriter, Read as _};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

use crate::protocol::{Message, Request, Response, read_frame, write_frame};

#[derive(Debug)]
pub struct WorkerProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

#[derive(Debug)]
pub enum WorkerHarnessError {
    Spawn(std::io::Error),
    MissingPipe(&'static str),
    Protocol(String),
    UnexpectedMessage(String),
    WorkerError { code: String, message: String },
    FatalExit(WorkerFatalExit),
    Wait(std::io::Error),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerFatalExit {
    pub status: String,
    pub stderr: String,
}

impl fmt::Display for WorkerHarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(err) => write!(f, "failed to spawn worker child: {err}"),
            Self::MissingPipe(which) => write!(f, "worker child missing {which} pipe"),
            Self::Protocol(err) => write!(f, "worker protocol failed: {err}"),
            Self::UnexpectedMessage(message) => write!(f, "worker sent unexpected message: {message}"),
            Self::WorkerError { code, message } => write!(f, "worker returned {code}: {message}"),
            Self::FatalExit(exit) => write!(f, "worker exited fatally with {}", exit.status),
            Self::Wait(err) => write!(f, "failed to wait for worker child: {err}"),
        }
    }
}

impl std::error::Error for WorkerHarnessError {}

impl WorkerProcess {
    pub fn spawn(binary: &Path) -> Result<Self, WorkerHarnessError> {
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("LEAN_ABORT_ON_PANIC", "1")
            .env("RUST_BACKTRACE", "0")
            .spawn()
            .map_err(WorkerHarnessError::Spawn)?;

        let stdin = child
            .stdin
            .take()
            .map(BufWriter::new)
            .ok_or(WorkerHarnessError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .map(BufReader::new)
            .ok_or(WorkerHarnessError::MissingPipe("stdout"))?;

        let mut worker = Self { child, stdin, stdout };
        worker.expect_handshake()?;
        Ok(worker)
    }

    pub fn health(&mut self) -> Result<(), WorkerHarnessError> {
        self.send_request(Request::Health)?;
        match self.read_response()? {
            Response::HealthOk => Ok(()),
            other => Err(Self::unexpected_response(&other)),
        }
    }

    pub fn load_fixture_capability(&mut self, fixture_root: &Path) -> Result<(), WorkerHarnessError> {
        self.send_request(Request::LoadFixtureCapability {
            fixture_root: path_string(fixture_root),
        })?;
        match self.read_response()? {
            Response::CapabilityLoaded => Ok(()),
            other => Err(Self::unexpected_response(&other)),
        }
    }

    pub fn call_fixture_mul(&mut self, fixture_root: &Path, lhs: u64, rhs: u64) -> Result<u64, WorkerHarnessError> {
        self.send_request(Request::CallFixtureMul {
            fixture_root: path_string(fixture_root),
            lhs,
            rhs,
        })?;
        match self.read_response()? {
            Response::U64 { value } => Ok(value),
            other => Err(Self::unexpected_response(&other)),
        }
    }

    pub fn terminate(mut self) -> Result<ExitStatus, WorkerHarnessError> {
        self.send_request(Request::Terminate)?;
        match self.read_response()? {
            Response::Terminating => self.child.wait().map_err(WorkerHarnessError::Wait),
            other => Err(Self::unexpected_response(&other)),
        }
    }

    pub fn trigger_lean_panic(mut self, fixture_root: &Path) -> Result<WorkerFatalExit, WorkerHarnessError> {
        self.send_request(Request::TriggerLeanPanic {
            fixture_root: path_string(fixture_root),
        })?;
        match read_frame(&mut self.stdout) {
            Ok(frame) => match frame.message {
                Message::Response(response) => Err(Self::unexpected_response(&response)),
                other => Err(WorkerHarnessError::UnexpectedMessage(format!("{other:?}"))),
            },
            Err(err) => {
                if !err.is_eof() {
                    return Err(WorkerHarnessError::Protocol(err.to_string()));
                }
                let status = self.child.wait().map_err(WorkerHarnessError::Wait)?;
                let mut stderr = String::new();
                if let Some(mut pipe) = self.child.stderr.take() {
                    drop(pipe.read_to_string(&mut stderr));
                }
                let fatal = WorkerFatalExit {
                    status: status.to_string(),
                    stderr,
                };
                if status.success() {
                    return Err(WorkerHarnessError::UnexpectedMessage(
                        "panic request closed the pipe but exited successfully".to_owned(),
                    ));
                }
                Ok(fatal)
            }
        }
    }

    fn expect_handshake(&mut self) -> Result<(), WorkerHarnessError> {
        let frame = read_frame(&mut self.stdout).map_err(|err| WorkerHarnessError::Protocol(err.to_string()))?;
        match frame.message {
            Message::Handshake { protocol_version, .. } if protocol_version == crate::protocol::PROTOCOL_VERSION => {
                Ok(())
            }
            other => Err(WorkerHarnessError::UnexpectedMessage(format!("{other:?}"))),
        }
    }

    fn send_request(&mut self, request: Request) -> Result<(), WorkerHarnessError> {
        write_frame(&mut self.stdin, Message::Request(request))
            .map_err(|err| WorkerHarnessError::Protocol(err.to_string()))
    }

    fn read_response(&mut self) -> Result<Response, WorkerHarnessError> {
        let frame = read_frame(&mut self.stdout).map_err(|err| WorkerHarnessError::Protocol(err.to_string()))?;
        match frame.message {
            Message::Response(Response::Error { code, message }) => {
                Err(WorkerHarnessError::WorkerError { code, message })
            }
            Message::Response(response) => Ok(response),
            other => Err(WorkerHarnessError::UnexpectedMessage(format!("{other:?}"))),
        }
    }

    fn unexpected_response(response: &Response) -> WorkerHarnessError {
        WorkerHarnessError::UnexpectedMessage(format!("{response:?}"))
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
