use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    DataRow(DataRow),
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
    RunDataStream {
        export: String,
        request_json: String,
        progress: bool,
    },
    CapabilityMetadata {
        export: String,
        request_json: String,
    },
    CapabilityDoctor {
        export: String,
        request_json: String,
    },
    // Private harness requests used to prove streaming frame behavior before
    // prompt 63 exposes a public row sink API.
    EmitTestRows {
        streams: Vec<String>,
    },
    EmitTestRowsThenExit,
    EmitTestRowsThenPanic,
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
    StreamComplete { summary: StreamSummary },
    StreamExportFailed { status_byte: u8 },
    StreamCallbackFailed { status_byte: u8, description: String },
    StreamRowMalformed { message: String },
    CapabilityMetadata { metadata: WorkerCapabilityMetadata },
    CapabilityDoctor { report: WorkerDoctorReport },
    CapabilityMetadataMalformed { message: String },
    CapabilityDoctorMalformed { message: String },
    RowsComplete { count: u64 },
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
pub(crate) struct DataRow {
    pub(crate) stream: String,
    pub(crate) sequence: u64,
    pub(crate) payload: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StreamSummary {
    pub(crate) total_rows: u64,
    pub(crate) per_stream_counts: BTreeMap<String, u64>,
    pub(crate) elapsed_micros: u64,
    pub(crate) metadata: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerCapabilityMetadata {
    pub(crate) commands: Vec<WorkerCommandMetadata>,
    pub(crate) capabilities: Vec<WorkerCapabilityFact>,
    pub(crate) lean_version: Option<String>,
    pub(crate) extra: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerCommandMetadata {
    pub(crate) name: String,
    pub(crate) version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerCapabilityFact {
    pub(crate) name: String,
    pub(crate) version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerDoctorReport {
    pub(crate) diagnostics: Vec<WorkerDoctorDiagnostic>,
    pub(crate) metadata: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkerDoctorDiagnostic {
    pub(crate) severity: WorkerDoctorSeverity,
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) details: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkerDoctorSeverity {
    Pass,
    Warning,
    Error,
}

impl StreamSummary {
    pub(crate) fn new(
        total_rows: u64,
        per_stream_counts: BTreeMap<String, u64>,
        elapsed: Duration,
        metadata: Option<Value>,
    ) -> Self {
        Self {
            total_rows,
            per_stream_counts,
            elapsed_micros: elapsed.as_micros().try_into().unwrap_or(u64::MAX),
            metadata,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct DataRowEmitter {
    sequences: BTreeMap<String, u64>,
    count: u64,
}

impl DataRowEmitter {
    pub(crate) fn next(&mut self, stream: impl Into<String>, payload: Value) -> DataRow {
        let stream = stream.into();
        let sequence = self.sequences.entry(stream.clone()).or_insert(0);
        let row = DataRow {
            stream,
            sequence: *sequence,
            payload,
        };
        *sequence = sequence.saturating_add(1);
        self.count = self.count.saturating_add(1);
        row
    }

    #[cfg(test)]
    pub(crate) fn emit(
        &mut self,
        writer: &mut impl Write,
        stream: impl Into<String>,
        payload: Value,
    ) -> Result<(), ProtocolError> {
        let row = self.next(stream, payload);
        write_frame(writer, Message::DataRow(row))
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    pub(crate) fn per_stream_counts(&self) -> BTreeMap<String, u64> {
        self.sequences.clone()
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use std::io::Cursor;

    use serde_json::json;

    use super::{DataRow, DataRowEmitter, MAX_FRAME_BYTES, Message, ProtocolError, Response, read_frame, write_frame};

    #[test]
    fn data_row_round_trips_through_length_delimited_frame() {
        let row = DataRow {
            stream: "rows".to_owned(),
            sequence: 7,
            payload: json!({ "name": "Nat.add", "score": 3 }),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, Message::DataRow(row.clone())).expect("data row writes");

        let frame = read_frame(&mut Cursor::new(bytes)).expect("data row reads");
        assert_eq!(frame.message, Message::DataRow(row));
    }

    #[test]
    fn data_row_emitter_assigns_per_stream_sequences() {
        let mut emitter = DataRowEmitter::default();
        let mut bytes = Vec::new();
        emitter
            .emit(&mut bytes, "rows", json!({ "i": 0 }))
            .expect("first row writes");
        emitter
            .emit(&mut bytes, "warnings", json!({ "i": 1 }))
            .expect("second row writes");
        emitter
            .emit(&mut bytes, "rows", json!({ "i": 2 }))
            .expect("third row writes");
        assert_eq!(emitter.count(), 3);

        let mut cursor = Cursor::new(bytes);
        let rows = [
            read_frame(&mut cursor).expect("first row reads"),
            read_frame(&mut cursor).expect("second row reads"),
            read_frame(&mut cursor).expect("third row reads"),
        ];
        assert_eq!(
            rows.map(|frame| frame.message),
            [
                Message::DataRow(DataRow {
                    stream: "rows".to_owned(),
                    sequence: 0,
                    payload: json!({ "i": 0 }),
                }),
                Message::DataRow(DataRow {
                    stream: "warnings".to_owned(),
                    sequence: 0,
                    payload: json!({ "i": 1 }),
                }),
                Message::DataRow(DataRow {
                    stream: "rows".to_owned(),
                    sequence: 1,
                    payload: json!({ "i": 2 }),
                }),
            ],
        );
    }

    #[test]
    fn oversized_data_row_is_rejected_before_write() {
        let row = DataRow {
            stream: "rows".to_owned(),
            sequence: 0,
            payload: json!({ "blob": "x".repeat(MAX_FRAME_BYTES as usize) }),
        };
        let mut bytes = Vec::new();
        let err = write_frame(&mut bytes, Message::DataRow(row)).expect_err("oversized frame is rejected");
        match err {
            ProtocolError::FrameTooLarge { len, max } => {
                assert!(len > max);
                assert_eq!(max, MAX_FRAME_BYTES);
            }
            other @ (ProtocolError::Io(_) | ProtocolError::Json(_) | ProtocolError::VersionMismatch { .. }) => {
                panic!("expected FrameTooLarge, got {other:?}");
            }
        }
    }

    #[test]
    fn oversized_data_row_is_rejected_before_read_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(MAX_FRAME_BYTES.saturating_add(1)).to_be_bytes());
        let err = read_frame(&mut Cursor::new(bytes)).expect_err("oversized frame is rejected");
        match err {
            ProtocolError::FrameTooLarge { len, max } => {
                assert_eq!(len, MAX_FRAME_BYTES + 1);
                assert_eq!(max, MAX_FRAME_BYTES);
            }
            other @ (ProtocolError::Io(_) | ProtocolError::Json(_) | ProtocolError::VersionMismatch { .. }) => {
                panic!("expected FrameTooLarge, got {other:?}");
            }
        }
    }

    #[test]
    fn malformed_frame_payload_is_protocol_error() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.push(b'{');
        let err = read_frame(&mut Cursor::new(bytes)).expect_err("malformed JSON is rejected");
        match err {
            ProtocolError::Json(_) => {}
            other @ (ProtocolError::Io(_)
            | ProtocolError::FrameTooLarge { .. }
            | ProtocolError::VersionMismatch { .. }) => {
                panic!("expected Json error, got {other:?}");
            }
        }
    }

    #[test]
    fn rows_complete_response_round_trips() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, Message::Response(Response::RowsComplete { count: 2 })).expect("rows complete writes");
        let frame = read_frame(&mut Cursor::new(bytes)).expect("rows complete reads");
        assert_eq!(frame.message, Message::Response(Response::RowsComplete { count: 2 }));
    }
}
