//! Integration-test harness for the worker process boundary.
//!
//! Gated behind the `test-support` Cargo feature so normal builds do not pull
//! in `std::process` plumbing. Used by the parent and child crates' end-to-end
//! tests to drive a spawned child binary through framed request/response
//! round-trips without re-implementing the codec.

#![allow(
    clippy::wildcard_enum_match_arm,
    reason = "test harness reports any unexpected protocol message with Debug"
)]

use std::fmt;
use std::io::{BufReader, BufWriter, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

use serde_json::Value;

use crate::protocol::{MAX_FRAME_BYTES, Message, Request, Response, read_frame, write_frame};
use crate::worker_exports::{fixture_mul_signature, fixture_panic_signature};

#[derive(Debug)]
pub struct WorkerProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    max_frame_bytes: u32,
}

#[derive(Debug)]
pub enum WorkerHarnessError {
    Spawn(std::io::Error),
    MissingPipe(&'static str),
    Protocol(String),
    CapabilityBuild(String),
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerDataRow {
    pub stream: String,
    pub sequence: u64,
    pub payload: Value,
}

impl fmt::Display for WorkerHarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(err) => write!(f, "failed to spawn worker child: {err}"),
            Self::MissingPipe(which) => write!(f, "worker child missing {which} pipe"),
            Self::Protocol(err) => write!(f, "worker protocol failed: {err}"),
            Self::CapabilityBuild(err) => write!(f, "failed to build fixture capability manifest: {err}"),
            Self::UnexpectedMessage(message) => write!(f, "worker sent unexpected message: {message}"),
            Self::WorkerError { code, message } => write!(f, "worker returned {code}: {message}"),
            Self::FatalExit(exit) => write!(f, "worker exited fatally with {}", exit.status),
            Self::Wait(err) => write!(f, "failed to wait for worker child: {err}"),
        }
    }
}

impl std::error::Error for WorkerHarnessError {}

impl WorkerProcess {
    /// Spawn `binary` as a worker child and read the handshake frame.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerHarnessError::Spawn`] if the OS could not start the
    /// process, [`WorkerHarnessError::MissingPipe`] if Cargo's piped stdio
    /// could not be claimed, [`WorkerHarnessError::Protocol`] for a
    /// codec-level handshake failure, or
    /// [`WorkerHarnessError::UnexpectedMessage`] if the first frame is not a
    /// matching handshake.
    pub fn spawn(binary: &Path) -> Result<Self, WorkerHarnessError> {
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("LEAN_ABORT_ON_PANIC", "1")
            // See `LeanWorkerConfig` docstring: `LEAN_BACKTRACE=0` prevents
            // Lean 4.30+ from calling back into Lean code from its panic
            // handler, which is unsafe when the child only initializes a
            // minimal Lean.
            .env("LEAN_BACKTRACE", "0")
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

        let mut worker = Self {
            child,
            stdin,
            stdout,
            max_frame_bytes: MAX_FRAME_BYTES,
        };
        worker.expect_handshake()?;
        worker.send_frame_limit()?;
        Ok(worker)
    }

    fn send_frame_limit(&mut self) -> Result<(), WorkerHarnessError> {
        write_frame(
            &mut self.stdin,
            Message::ConfigureFrameLimit {
                max_frame_bytes: self.max_frame_bytes,
            },
            self.max_frame_bytes,
        )
        .map_err(|err| WorkerHarnessError::Protocol(err.to_string()))
    }

    /// Send a `health` request and assert a `health_ok` response.
    ///
    /// # Errors
    ///
    /// Returns the protocol- or worker-level error variant if the round-trip
    /// fails or the child returned an unexpected response.
    pub fn health(&mut self) -> Result<(), WorkerHarnessError> {
        self.send_request(Request::Health)?;
        match self.read_response()? {
            Response::HealthOk => Ok(()),
            other => Err(Self::unexpected_response(&other)),
        }
    }

    /// Send `load_fixture_capability` rooted at `fixture_root` and assert a
    /// `capability_loaded` response.
    ///
    /// # Errors
    ///
    /// Returns the protocol- or worker-level error variant if the round-trip
    /// fails or the child returned an unexpected response.
    pub fn load_fixture_capability(&mut self, fixture_root: &Path) -> Result<(), WorkerHarnessError> {
        let manifest_path = fixture_capability_manifest(fixture_root)?;
        self.send_request(Request::LoadFixtureCapability {
            manifest_path: path_string(&manifest_path),
        })?;
        match self.read_response()? {
            Response::CapabilityLoaded => Ok(()),
            other => Err(Self::unexpected_response(&other)),
        }
    }

    /// Send `call_fixture_mul` for `(lhs, rhs)` against the fixture rooted at
    /// `fixture_root` and return the `u64` payload.
    ///
    /// # Errors
    ///
    /// Returns the protocol- or worker-level error variant if the round-trip
    /// fails or the child returned a non-`u64` response.
    pub fn call_fixture_mul(&mut self, fixture_root: &Path, lhs: u64, rhs: u64) -> Result<u64, WorkerHarnessError> {
        let manifest_path = fixture_capability_manifest(fixture_root)?;
        self.send_request(Request::CallFixtureMul {
            manifest_path: path_string(&manifest_path),
            lhs,
            rhs,
        })?;
        match self.read_response()? {
            Response::U64 { value } => Ok(value),
            other => Err(Self::unexpected_response(&other)),
        }
    }

    /// Send `terminate`, await the `terminating` response, and reap the
    /// child process.
    ///
    /// # Errors
    ///
    /// Returns the protocol- or worker-level error variant if the round-trip
    /// fails, or [`WorkerHarnessError::Wait`] if reaping the child fails.
    pub fn terminate(mut self) -> Result<ExitStatus, WorkerHarnessError> {
        self.send_request(Request::Terminate)?;
        match self.read_response()? {
            Response::Terminating => self.child.wait().map_err(WorkerHarnessError::Wait),
            other => Err(Self::unexpected_response(&other)),
        }
    }

    /// Trigger a deliberate Lean panic inside the child and return the
    /// observed fatal-exit envelope (status + captured stderr).
    ///
    /// # Errors
    ///
    /// Returns [`WorkerHarnessError::UnexpectedMessage`] if the child
    /// responds normally instead of tearing down, [`WorkerHarnessError::Wait`]
    /// if reaping the child fails, or [`WorkerHarnessError::Protocol`] for a
    /// codec-level failure during the teardown read.
    pub fn trigger_lean_panic(mut self, fixture_root: &Path) -> Result<WorkerFatalExit, WorkerHarnessError> {
        let manifest_path = fixture_capability_manifest(fixture_root)?;
        self.send_request(Request::TriggerLeanPanic {
            manifest_path: path_string(&manifest_path),
        })?;
        match read_frame(&mut self.stdout, self.max_frame_bytes) {
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

    /// Send `emit_test_rows` for the named streams and collect every row up
    /// to the terminal `rows_complete` response.
    ///
    /// # Errors
    ///
    /// Returns the protocol- or worker-level error variant if any frame
    /// reads fail, the row count disagrees with the terminal response, or
    /// the child returns an unexpected response.
    pub fn emit_test_rows(&mut self, streams: Vec<String>) -> Result<Vec<WorkerDataRow>, WorkerHarnessError> {
        self.send_request(Request::EmitTestRows { streams })?;
        self.read_rows_until_complete()
    }

    /// Send `emit_test_rows_then_exit` and collect rows until the child
    /// closes its pipe; expects a non-zero exit after at least one row.
    ///
    /// # Errors
    ///
    /// Returns the protocol- or worker-level error variant if the child
    /// exits cleanly, emits no rows, or returns an unexpected response.
    pub fn emit_rows_then_exit(mut self) -> Result<Vec<WorkerDataRow>, WorkerHarnessError> {
        self.send_request(Request::EmitTestRowsThenExit)?;
        let mut row_count = 0_u64;
        loop {
            match read_frame(&mut self.stdout, self.max_frame_bytes) {
                Ok(frame) => match frame.message {
                    Message::DataRow(_row) => {
                        row_count = row_count.saturating_add(1);
                    }
                    Message::Response(response) => return Err(Self::unexpected_response(&response)),
                    other => return Err(WorkerHarnessError::UnexpectedMessage(format!("{other:?}"))),
                },
                Err(err) if err.is_eof() => {
                    let status = self.child.wait().map_err(WorkerHarnessError::Wait)?;
                    if row_count == 0 {
                        return Err(WorkerHarnessError::Protocol(
                            "worker exited before sending any row frame".to_owned(),
                        ));
                    }
                    if status.success() {
                        return Err(WorkerHarnessError::Protocol(
                            "worker exited before terminal row response".to_owned(),
                        ));
                    }
                    return Err(WorkerHarnessError::FatalExit(WorkerFatalExit {
                        status: status.to_string(),
                        stderr: String::new(),
                    }));
                }
                Err(err) => return Err(WorkerHarnessError::Protocol(err.to_string())),
            }
        }
    }

    /// Send `emit_test_rows_then_panic` and collect rows until the child
    /// panics; returns the captured fatal-exit envelope.
    ///
    /// # Errors
    ///
    /// Returns the protocol- or worker-level error variant if the child
    /// exits cleanly, emits no rows, or returns an unexpected response.
    pub fn emit_rows_then_panic(mut self) -> Result<Vec<WorkerDataRow>, WorkerHarnessError> {
        self.send_request(Request::EmitTestRowsThenPanic)?;
        let mut row_count = 0_u64;
        loop {
            match read_frame(&mut self.stdout, self.max_frame_bytes) {
                Ok(frame) => match frame.message {
                    Message::DataRow(_row) => {
                        row_count = row_count.saturating_add(1);
                    }
                    Message::Response(response) => return Err(Self::unexpected_response(&response)),
                    other => return Err(WorkerHarnessError::UnexpectedMessage(format!("{other:?}"))),
                },
                Err(err) if err.is_eof() => {
                    let status = self.child.wait().map_err(WorkerHarnessError::Wait)?;
                    let mut stderr = String::new();
                    if let Some(mut pipe) = self.child.stderr.take() {
                        drop(pipe.read_to_string(&mut stderr));
                    }
                    if status.success() {
                        return Err(WorkerHarnessError::UnexpectedMessage(
                            "panic row request exited successfully".to_owned(),
                        ));
                    }
                    if row_count == 0 {
                        return Err(WorkerHarnessError::UnexpectedMessage(
                            "panic row request exited before sending any row frame".to_owned(),
                        ));
                    }
                    return Err(WorkerHarnessError::FatalExit(WorkerFatalExit {
                        status: status.to_string(),
                        stderr,
                    }));
                }
                Err(err) => return Err(WorkerHarnessError::Protocol(err.to_string())),
            }
        }
    }

    fn expect_handshake(&mut self) -> Result<(), WorkerHarnessError> {
        let frame = read_frame(&mut self.stdout, self.max_frame_bytes)
            .map_err(|err| WorkerHarnessError::Protocol(err.to_string()))?;
        match frame.message {
            Message::Handshake { protocol_version, .. } if protocol_version == crate::protocol::PROTOCOL_VERSION => {
                Ok(())
            }
            other => Err(WorkerHarnessError::UnexpectedMessage(format!("{other:?}"))),
        }
    }

    fn send_request(&mut self, request: Request) -> Result<(), WorkerHarnessError> {
        write_frame(&mut self.stdin, Message::Request(request), self.max_frame_bytes)
            .map_err(|err| WorkerHarnessError::Protocol(err.to_string()))
    }

    fn read_response(&mut self) -> Result<Response, WorkerHarnessError> {
        let frame = read_frame(&mut self.stdout, self.max_frame_bytes)
            .map_err(|err| WorkerHarnessError::Protocol(err.to_string()))?;
        match frame.message {
            Message::Response(Response::Error { code, message }) => {
                Err(WorkerHarnessError::WorkerError { code, message })
            }
            Message::Response(response) => Ok(response),
            other => Err(WorkerHarnessError::UnexpectedMessage(format!("{other:?}"))),
        }
    }

    fn read_rows_until_complete(&mut self) -> Result<Vec<WorkerDataRow>, WorkerHarnessError> {
        let mut rows = Vec::new();
        loop {
            let frame = read_frame(&mut self.stdout, self.max_frame_bytes)
                .map_err(|err| WorkerHarnessError::Protocol(err.to_string()))?;
            match frame.message {
                Message::DataRow(row) => rows.push(WorkerDataRow {
                    stream: row.stream,
                    sequence: row.sequence,
                    payload: serde_json::from_str(row.payload.get())
                        .map_err(|err| WorkerHarnessError::Protocol(err.to_string()))?,
                }),
                Message::Response(Response::RowsComplete { count }) => {
                    let actual = u64::try_from(rows.len()).unwrap_or(u64::MAX);
                    if actual == count {
                        return Ok(rows);
                    }
                    return Err(WorkerHarnessError::UnexpectedMessage(format!(
                        "row count mismatch: terminal response reported {count}, received {actual}"
                    )));
                }
                Message::Response(Response::Error { code, message }) => {
                    return Err(WorkerHarnessError::WorkerError { code, message });
                }
                Message::Response(response) => return Err(Self::unexpected_response(&response)),
                other => return Err(WorkerHarnessError::UnexpectedMessage(format!("{other:?}"))),
            }
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

fn fixture_capability_manifest(fixture_root: &Path) -> Result<PathBuf, WorkerHarnessError> {
    let built = lean_toolchain::CargoLeanCapability::new(fixture_root, "LeanRsFixture")
        .package("lean_rs_fixture")
        .module("LeanRsFixture")
        .export_signature(fixture_mul_signature("lean_rs_fixture_u64_mul"))
        .export_signature(fixture_panic_signature("lean_rs_fixture_panic_unit"))
        .build_quiet()
        .map_err(|diagnostic| WorkerHarnessError::CapabilityBuild(diagnostic.to_string()))?;
    Ok(built.manifest_path().to_path_buf())
}
