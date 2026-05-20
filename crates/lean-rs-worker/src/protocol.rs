use std::fmt;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

pub(crate) const PROTOCOL_VERSION: u16 = 1;
const MAX_FRAME_BYTES: u32 = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Frame {
    pub(crate) version: u16,
    pub(crate) message: Message,
}

impl Frame {
    pub(crate) fn new(message: Message) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            message,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Message {
    Handshake {
        worker_version: String,
        protocol_version: u16,
    },
    Request(Request),
    Response(Response),
    Diagnostic(Diagnostic),
    ProgressTick(ProgressTick),
    FatalExit(FatalExit),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Request {
    Health,
    LoadFixtureCapability {
        fixture_root: String,
    },
    CallFixtureMul {
        fixture_root: String,
        lhs: u64,
        rhs: u64,
    },
    TriggerLeanPanic {
        fixture_root: String,
    },
    OpenHostSession {
        project_root: String,
        package: String,
        lib_name: String,
        imports: Vec<String>,
    },
    Elaborate {
        source: String,
        options: WorkerElabOptions,
    },
    KernelCheck {
        source: String,
        options: WorkerElabOptions,
        progress: bool,
    },
    DeclarationKinds {
        names: Vec<String>,
        progress: bool,
    },
    DeclarationNames {
        names: Vec<String>,
        progress: bool,
    },
    Terminate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Response {
    HealthOk,
    CapabilityLoaded,
    U64 { value: u64 },
    HostSessionOpened,
    Elaboration { outcome: WorkerElabOutcome },
    KernelCheck { outcome: WorkerKernelOutcome },
    Strings { values: Vec<String> },
    Terminating,
    Error { code: String, message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Diagnostic {
    pub(crate) code: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ProgressTick {
    pub(crate) phase: String,
    pub(crate) current: u64,
    pub(crate) total: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerElabOptions {
    pub(crate) namespace_context: String,
    pub(crate) file_label: String,
    pub(crate) heartbeat_limit: u64,
    pub(crate) diagnostic_byte_limit: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerElabOutcome {
    pub(crate) success: bool,
    pub(crate) diagnostics: Vec<WorkerDiagnostic>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerKernelOutcome {
    pub(crate) status: WorkerKernelStatus,
    pub(crate) diagnostics: Vec<WorkerDiagnostic>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkerKernelStatus {
    Checked,
    Rejected,
    Unavailable,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerDiagnostic {
    pub(crate) severity: String,
    pub(crate) message: String,
    pub(crate) file_label: String,
    pub(crate) line: Option<u32>,
    pub(crate) column: Option<u32>,
    pub(crate) end_line: Option<u32>,
    pub(crate) end_column: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FatalExit {
    pub(crate) status: String,
    pub(crate) stderr: String,
}

#[derive(Debug)]
pub(crate) enum ProtocolError {
    Io(io::Error),
    Json(serde_json::Error),
    FrameTooLarge { len: u32, max: u32 },
    VersionMismatch { expected: u16, actual: u16 },
}

impl ProtocolError {
    pub(crate) fn is_eof(&self) -> bool {
        matches!(self, Self::Io(err) if err.kind() == io::ErrorKind::UnexpectedEof)
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "worker protocol I/O failed: {err}"),
            Self::Json(err) => write!(f, "worker protocol JSON decode failed: {err}"),
            Self::FrameTooLarge { len, max } => {
                write!(f, "worker protocol frame too large: {len} bytes exceeds {max}")
            }
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "worker protocol version mismatch: expected {expected}, received {actual}"
                )
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<io::Error> for ProtocolError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub(crate) fn write_frame(writer: &mut impl Write, message: Message) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(&Frame::new(message))?;
    let len = u32::try_from(bytes.len()).map_err(|_| ProtocolError::FrameTooLarge {
        len: u32::MAX,
        max: MAX_FRAME_BYTES,
    })?;
    if len > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn read_frame(reader: &mut impl Read) -> Result<Frame, ProtocolError> {
    let mut len_bytes = [0_u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_be_bytes(len_bytes);
    if len > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    let mut bytes = vec![0_u8; len as usize];
    reader.read_exact(&mut bytes)?;
    let frame: Frame = serde_json::from_slice(&bytes)?;
    if frame.version != PROTOCOL_VERSION {
        return Err(ProtocolError::VersionMismatch {
            expected: PROTOCOL_VERSION,
            actual: frame.version,
        });
    }
    Ok(frame)
}
