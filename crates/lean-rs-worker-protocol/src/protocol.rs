//! Length-delimited frame codec and message payload types for the
//! parent ↔ child worker process boundary.
//!
//! ## Additive evolution
//!
//! Every public enum here is `#[non_exhaustive]` so the wire format can gain
//! a new request, response, or message kind without forcing a semver-major
//! bump on consumers. Most structs are also `#[non_exhaustive]` and expose
//! `pub fn new(...)` constructors so the shapes can grow fields without
//! breaking external builders. The exception is [`DataRow`], which is built
//! so frequently with struct-literal syntax (tests, harnesses, fakes) that
//! the ergonomic cost of `#[non_exhaustive]` outweighs the additive-evolution
//! benefit; the wire schema for a data row is also already fixed by the
//! stream contract.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::types::{
    LeanWorkerCapabilityMetadata, LeanWorkerDeclarationFilter, LeanWorkerDeclarationInspectionRequest,
    LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationRow, LeanWorkerDeclarationSearch,
    LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationType, LeanWorkerDeclarationVerificationRequest,
    LeanWorkerDeclarationVerificationResult, LeanWorkerDoctorReport, LeanWorkerElabOptions, LeanWorkerElabResult,
    LeanWorkerImportStats, LeanWorkerKernelResult, LeanWorkerMetaResult, LeanWorkerMetaTransparency,
    LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryOutcome,
    LeanWorkerModuleQuerySelector, LeanWorkerOutputBudgets, LeanWorkerProofAttemptRequest,
    LeanWorkerProofAttemptResult, LeanWorkerRendered, LeanWorkerSessionImportProfile,
};

/// Wire protocol version negotiated between parent and child during the
/// handshake frame. Bump only on a breaking wire change.
pub const PROTOCOL_VERSION: u16 = 10;

/// Default per-frame size limit applied by the parent when no explicit cap is
/// configured on the capability builder.
///
/// The cap is a parent-side policy decision negotiated to the child at
/// handshake time via [`Message::ConfigureFrameLimit`]. Both [`write_frame`]
/// and [`read_frame`] reject frames whose serialised JSON payload exceeds the
/// cap passed in, so a runaway producer cannot make the peer allocate without
/// bound. The cap is per-connection—set once at handshake, applied to every
/// subsequent frame in both directions.
pub const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// Floor on the configurable frame cap. Trivial requests and the handshake
/// itself must fit inside this; callers cannot configure smaller.
pub const MIN_FRAME_BYTES: u32 = 64 * 1024;

/// Ceiling on the configurable frame cap. Prevents callers from defeating the
/// memory-safety policy by passing an absurdly large value.
pub const MAX_FRAME_BYTES_HARD_CAP: u32 = 256 * 1024 * 1024;

/// Versioned envelope around a single protocol [`Message`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Frame {
    /// Protocol version the sender used. Receivers reject mismatches.
    pub version: u16,
    /// Inner message payload.
    pub message: Message,
}

impl Frame {
    /// Wrap `message` in a frame tagged with the current [`PROTOCOL_VERSION`].
    #[must_use]
    pub fn new(message: Message) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            message,
        }
    }
}

/// One protocol message exchanged over the worker boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "body", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Message {
    /// Sent by the child immediately after spawn to advertise its version and
    /// supported protocol revision.
    Handshake {
        /// `lean-rs-worker-child` package version.
        worker_version: String,
        /// Protocol version the child speaks. Parent rejects mismatches.
        protocol_version: u16,
    },
    /// Sent by the parent immediately after the handshake frame to negotiate
    /// the per-frame size cap for the remainder of this connection.
    ///
    /// The parent owns the memory-safety policy: it clamps the requested cap
    /// into <code>[[MIN_FRAME_BYTES], [MAX_FRAME_BYTES_HARD_CAP]]</code>
    /// before sending. The child installs the value as-is.
    ConfigureFrameLimit {
        /// Per-frame byte cap applied in both directions for this connection.
        max_frame_bytes: u32,
    },
    /// Parent → child request frame.
    Request(Request),
    /// Child → parent terminal response for one request.
    Response(Response),
    /// Child → parent intermediate diagnostic frame.
    Diagnostic(Diagnostic),
    /// Child → parent intermediate progress frame.
    ProgressTick(ProgressTick),
    /// Child → parent streaming data row frame.
    DataRow(DataRow),
    /// Child → parent fatal exit notification carrying the captured stderr
    /// just before the child process tears down.
    FatalExit(FatalExit),
}

/// Parent-issued worker request body.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Request {
    Health,
    LoadFixtureCapability {
        manifest_path: String,
    },
    CallFixtureMul {
        manifest_path: String,
        lhs: u64,
        rhs: u64,
    },
    TriggerLeanPanic {
        manifest_path: String,
    },
    OpenHostSession {
        project_root: String,
        mode: HostSessionMode,
        imports: Vec<String>,
        import_profile: LeanWorkerSessionImportProfile,
    },
    Elaborate {
        source: String,
        options: LeanWorkerElabOptions,
    },
    KernelCheck {
        source: String,
        options: LeanWorkerElabOptions,
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
    JsonCommand {
        export: String,
        request_json: String,
    },
    InferType {
        source: String,
        options: LeanWorkerElabOptions,
    },
    Whnf {
        source: String,
        options: LeanWorkerElabOptions,
    },
    IsDefEq {
        lhs: String,
        rhs: String,
        transparency: LeanWorkerMetaTransparency,
        options: LeanWorkerElabOptions,
    },
    Describe {
        name: String,
    },
    SearchDeclarations {
        search: LeanWorkerDeclarationSearch,
    },
    DeclarationType {
        name: String,
        max_bytes: usize,
    },
    InspectDeclaration {
        request: LeanWorkerDeclarationInspectionRequest,
    },
    AttemptProof {
        request: LeanWorkerProofAttemptRequest,
        options: LeanWorkerElabOptions,
        progress: bool,
    },
    VerifyDeclaration {
        request: LeanWorkerDeclarationVerificationRequest,
        options: LeanWorkerElabOptions,
        progress: bool,
    },
    ListDeclarationsStrings {
        filter: LeanWorkerDeclarationFilter,
        progress: bool,
    },
    DescribeBulk {
        names: Vec<String>,
        progress: bool,
    },
    ProcessModuleQuery {
        source: String,
        query: LeanWorkerModuleQuery,
        options: LeanWorkerElabOptions,
    },
    ProcessModuleQueryBatch {
        source: String,
        selectors: Vec<LeanWorkerModuleQuerySelector>,
        budgets: LeanWorkerOutputBudgets,
        options: LeanWorkerElabOptions,
    },
    ClearModuleSnapshotCache,
    // Private harness requests that exercise streaming frame behavior.
    // Not part of the public row sink API.
    EmitTestRows {
        streams: Vec<String>,
    },
    EmitTestRowsThenExit,
    EmitTestRowsThenPanic,
    Terminate,
}

/// How the worker child should load host-session capabilities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostSessionMode {
    /// Open a user capability dylib and the bundled host shims.
    Capability {
        package: String,
        lib_name: String,
        manifest_path: Option<String>,
    },
    /// Open only the bundled host shims.
    ShimsOnly,
}

/// Child-issued terminal response body for one [`Request`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Response {
    HealthOk,
    CapabilityLoaded,
    U64 {
        value: u64,
    },
    HostSessionOpened {
        import_stats: LeanWorkerImportStats,
    },
    Elaboration {
        outcome: LeanWorkerElabResult,
    },
    KernelCheck {
        outcome: LeanWorkerKernelResult,
    },
    Strings {
        values: Vec<String>,
    },
    StreamComplete {
        summary: StreamSummary,
    },
    StreamExportFailed {
        status_byte: u8,
    },
    StreamCallbackFailed {
        status_byte: u8,
        description: String,
    },
    StreamRowMalformed {
        message: String,
    },
    CapabilityMetadata {
        metadata: LeanWorkerCapabilityMetadata,
    },
    CapabilityDoctor {
        report: LeanWorkerDoctorReport,
    },
    CapabilityMetadataMalformed {
        message: String,
    },
    CapabilityDoctorMalformed {
        message: String,
    },
    JsonCommand {
        response_json: String,
    },
    MetaExpr {
        result: LeanWorkerMetaResult<LeanWorkerRendered>,
    },
    MetaBool {
        result: LeanWorkerMetaResult<bool>,
    },
    Declaration {
        row: Option<LeanWorkerDeclarationRow>,
    },
    DeclarationSearch {
        result: LeanWorkerDeclarationSearchResult,
    },
    DeclarationType {
        row: Option<LeanWorkerDeclarationType>,
    },
    DeclarationInspection {
        result: LeanWorkerDeclarationInspectionResult,
    },
    ProofAttempt {
        result: LeanWorkerProofAttemptResult,
    },
    DeclarationVerification {
        result: LeanWorkerDeclarationVerificationResult,
    },
    DeclarationBulk {
        rows: Vec<LeanWorkerDeclarationRow>,
    },
    ProcessModuleQuery {
        outcome: LeanWorkerModuleQueryOutcome,
    },
    ProcessModuleQueryBatch {
        outcome: LeanWorkerModuleQueryBatchOutcome,
    },
    ModuleSnapshotCacheCleared {
        result: crate::types::LeanWorkerModuleSnapshotCacheClearResult,
    },
    RowsComplete {
        count: u64,
    },
    Terminating,
    Error {
        code: String,
        message: String,
    },
}

/// Intermediate diagnostic frame emitted by the child during a request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Diagnostic {
    /// Stable diagnostic code identifier.
    pub code: String,
    /// Bounded human-readable diagnostic message.
    pub message: String,
}

impl Diagnostic {
    /// Build a diagnostic frame payload.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Intermediate progress frame emitted by the child during a long-running
/// request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ProgressTick {
    /// Phase name the child is reporting progress for.
    pub phase: String,
    /// Items completed so far in this phase.
    pub current: u64,
    /// Total expected items in this phase, if known.
    pub total: Option<u64>,
}

impl ProgressTick {
    /// Build a progress-tick frame payload.
    #[must_use]
    pub fn new(phase: impl Into<String>, current: u64, total: Option<u64>) -> Self {
        Self {
            phase: phase.into(),
            current,
            total,
        }
    }
}

/// One row in a streaming response.
///
/// Construction goes through [`DataRowEmitter::next`] in the child runtime;
/// direct struct-literal construction is permitted in tests and harnesses.
/// This struct intentionally stays exhaustive: see the module-level note on
/// additive evolution.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DataRow {
    /// Logical stream this row belongs to.
    pub stream: String,
    /// Per-stream monotonically increasing sequence number.
    pub sequence: u64,
    /// Opaque JSON payload (deserialised lazily by the parent).
    pub payload: Box<RawValue>,
}

impl PartialEq for DataRow {
    fn eq(&self, other: &Self) -> bool {
        self.stream == other.stream && self.sequence == other.sequence && self.payload.get() == other.payload.get()
    }
}

impl Eq for DataRow {}

/// Terminal stream-completion summary returned alongside a streaming response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct StreamSummary {
    /// Total rows emitted across all streams.
    pub total_rows: u64,
    /// Per-stream row counts at completion.
    pub per_stream_counts: BTreeMap<String, u64>,
    /// Child-side elapsed time in microseconds.
    pub elapsed_micros: u64,
    /// Optional downstream-defined terminal metadata.
    pub metadata: Option<Value>,
}

impl StreamSummary {
    /// Build a stream-completion summary, clamping the elapsed duration into
    /// the `u64` micros field.
    #[must_use]
    pub fn new(
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

/// Stateful emitter that assigns per-stream sequence numbers and tracks the
/// running row count for the terminal [`StreamSummary`].
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct DataRowEmitter {
    sequences: BTreeMap<String, u64>,
    count: u64,
}

impl DataRowEmitter {
    /// Allocate the next [`DataRow`] for `stream`, advancing the per-stream
    /// sequence and the overall count.
    pub fn next(&mut self, stream: impl Into<String>, payload: Box<RawValue>) -> DataRow {
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
    fn emit(
        &mut self,
        writer: &mut impl Write,
        stream: impl Into<String>,
        payload: &Value,
    ) -> Result<(), ProtocolError> {
        let row = self.next(stream, serde_json::value::to_raw_value(payload)?);
        write_frame(writer, Message::DataRow(row), MAX_FRAME_BYTES)
    }

    /// Total rows emitted across all streams.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Snapshot of per-stream row counts.
    #[must_use]
    pub fn per_stream_counts(&self) -> BTreeMap<String, u64> {
        self.sequences.clone()
    }
}

/// Final frame the child writes before it tears down on an unrecoverable
/// failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct FatalExit {
    /// Stringified `ExitStatus` of the child process.
    pub status: String,
    /// Captured stderr tail at fatal-exit time.
    pub stderr: String,
}

impl FatalExit {
    /// Build a fatal-exit frame payload.
    #[must_use]
    pub fn new(status: impl Into<String>, stderr: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            stderr: stderr.into(),
        }
    }
}

/// Failure modes the codec can produce while reading or writing a frame.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProtocolError {
    /// Underlying I/O failure (pipe closed, partial read, etc.).
    Io(io::Error),
    /// JSON serialisation or deserialisation failure.
    Json(serde_json::Error),
    /// A frame body exceeded [`MAX_FRAME_BYTES`].
    FrameTooLarge {
        /// Observed frame size in bytes.
        len: u32,
        /// Maximum allowed frame size.
        max: u32,
    },
    /// Peer's frame version did not match this binary's [`PROTOCOL_VERSION`].
    VersionMismatch {
        /// Version this binary expected.
        expected: u16,
        /// Version the peer used.
        actual: u16,
    },
}

impl ProtocolError {
    /// Whether the underlying I/O error indicates the peer's pipe was closed
    /// (`UnexpectedEof`). Used by callers to distinguish a clean fatal exit
    /// from a true protocol failure.
    #[must_use]
    pub fn is_eof(&self) -> bool {
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

/// Serialise `message` as a length-delimited JSON frame to `writer`.
///
/// `max_frame_bytes` is the per-frame cap negotiated for this connection.
/// Until the handshake completes, callers pass [`MAX_FRAME_BYTES`] as the
/// default; afterwards the supervisor passes the
/// [`Message::ConfigureFrameLimit`] value installed on the connection.
///
/// # Errors
///
/// Returns [`ProtocolError::FrameTooLarge`] if the serialised body would
/// exceed `max_frame_bytes`, or the underlying [`ProtocolError::Io`] /
/// [`ProtocolError::Json`] for codec failures.
pub fn write_frame(writer: &mut impl Write, message: Message, max_frame_bytes: u32) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(&Frame::new(message))?;
    let len = u32::try_from(bytes.len()).map_err(|_| ProtocolError::FrameTooLarge {
        len: u32::MAX,
        max: max_frame_bytes,
    })?;
    if len > max_frame_bytes {
        return Err(ProtocolError::FrameTooLarge {
            len,
            max: max_frame_bytes,
        });
    }
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

/// Read one length-delimited JSON frame from `reader`.
///
/// `max_frame_bytes` is the per-frame cap negotiated for this connection. See
/// [`write_frame`] for the back-compat default and post-handshake semantics.
///
/// # Errors
///
/// Returns [`ProtocolError::FrameTooLarge`] if the framed length exceeds
/// `max_frame_bytes` (rejected before allocation),
/// [`ProtocolError::VersionMismatch`] if the peer's version does not match
/// [`PROTOCOL_VERSION`], or the underlying [`ProtocolError::Io`] /
/// [`ProtocolError::Json`] for codec failures.
pub fn read_frame(reader: &mut impl Read, max_frame_bytes: u32) -> Result<Frame, ProtocolError> {
    let mut len_bytes = [0_u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_be_bytes(len_bytes);
    if len > max_frame_bytes {
        return Err(ProtocolError::FrameTooLarge {
            len,
            max: max_frame_bytes,
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
    use serde_json::value::RawValue;

    use super::{
        DataRow, DataRowEmitter, MAX_FRAME_BYTES, MAX_FRAME_BYTES_HARD_CAP, MIN_FRAME_BYTES, Message, ProtocolError,
        Request, Response, read_frame, write_frame,
    };
    use crate::types::{
        LeanWorkerDeclarationFilter, LeanWorkerDeclarationFlags, LeanWorkerDeclarationInspection,
        LeanWorkerDeclarationInspectionFields, LeanWorkerDeclarationInspectionRequest,
        LeanWorkerDeclarationInspectionResult, LeanWorkerDeclarationNameMatch, LeanWorkerDeclarationProofSearchFacts,
        LeanWorkerDeclarationSearch, LeanWorkerDeclarationSearchBias, LeanWorkerDeclarationSearchFacts,
        LeanWorkerDeclarationSearchPruning, LeanWorkerDeclarationSearchResult, LeanWorkerDeclarationSearchRow,
        LeanWorkerDeclarationSearchScope, LeanWorkerDeclarationSearchTimings, LeanWorkerDeclarationTargetInfo,
        LeanWorkerDeclarationVerificationFacts, LeanWorkerDeclarationVerificationRequest,
        LeanWorkerDeclarationVerificationResult, LeanWorkerDeclarationVerificationStatus,
        LeanWorkerDeclarationVerificationTarget, LeanWorkerDerivedWorkFacts, LeanWorkerElabFailure,
        LeanWorkerElabOptions, LeanWorkerModuleCacheStatus, LeanWorkerModuleQuery, LeanWorkerModuleQueryBatchEnvelope,
        LeanWorkerModuleQueryBatchItem, LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryBatchResult,
        LeanWorkerModuleQueryCacheFacts, LeanWorkerModuleQueryOutcome, LeanWorkerModuleQueryResult,
        LeanWorkerModuleQuerySelector, LeanWorkerModuleQueryTimings, LeanWorkerModuleSourceSpan,
        LeanWorkerOutputBudgets, LeanWorkerProofAttemptEnvelope, LeanWorkerProofAttemptRequest,
        LeanWorkerProofAttemptResult, LeanWorkerProofAttemptRow, LeanWorkerProofAttemptStatus,
        LeanWorkerProofCandidate, LeanWorkerProofEditTarget, LeanWorkerProofPositionSelector,
        LeanWorkerProofPositionSummary, LeanWorkerProofStateResult, LeanWorkerRenderedInfo, LeanWorkerRendering,
        LeanWorkerResourceExhaustedFacts, LeanWorkerSorryPolicy, LeanWorkerSourceRange, LeanWorkerTypeAtResult,
    };

    fn raw_json(value: &serde_json::Value) -> Box<RawValue> {
        serde_json::value::to_raw_value(value).expect("test JSON converts to raw value")
    }

    fn assert_frame_round_trips(message: &Message) {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, message.clone(), MAX_FRAME_BYTES).expect("frame writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("frame reads");
        assert_eq!(&frame.message, message);
    }

    fn declaration_target_info_fixture(declaration_name: &str) -> LeanWorkerDeclarationTargetInfo {
        let span = LeanWorkerModuleSourceSpan {
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 10,
        };
        let short_name = declaration_name.rsplit('.').next().unwrap_or(declaration_name);
        LeanWorkerDeclarationTargetInfo {
            short_name: short_name.to_owned(),
            declaration_name: declaration_name.to_owned(),
            namespace_name: declaration_name
                .strip_suffix(&format!(".{short_name}"))
                .unwrap_or("")
                .to_owned(),
            declaration_kind: "theorem".to_owned(),
            declaration_span: span.clone(),
            name_span: span.clone(),
            body_span: span,
        }
    }

    fn verification_facts_fixture(
        candidates: Vec<LeanWorkerDeclarationTargetInfo>,
        axioms_available: bool,
    ) -> LeanWorkerDeclarationVerificationFacts {
        LeanWorkerDeclarationVerificationFacts {
            target: None,
            diagnostics: LeanWorkerElabFailure {
                diagnostics: Vec::new(),
                truncated: false,
            },
            unresolved_goals: Vec::new(),
            contains_sorry: false,
            contains_admit: false,
            contains_sorry_ax: false,
            axioms: Vec::new(),
            axioms_truncated: false,
            output_truncated: false,
            candidates,
            axioms_available,
        }
    }

    #[test]
    fn data_row_round_trips_through_length_delimited_frame() {
        let row = DataRow {
            stream: "rows".to_owned(),
            sequence: 7,
            payload: raw_json(&json!({ "name": "Nat.add", "score": 3 })),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, Message::DataRow(row.clone()), MAX_FRAME_BYTES).expect("data row writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("data row reads");
        assert_eq!(frame.message, Message::DataRow(row));
    }

    #[test]
    fn data_row_emitter_assigns_per_stream_sequences() {
        let mut emitter = DataRowEmitter::default();
        let mut bytes = Vec::new();
        emitter
            .emit(&mut bytes, "rows", &json!({ "i": 0 }))
            .expect("first row writes");
        emitter
            .emit(&mut bytes, "warnings", &json!({ "i": 1 }))
            .expect("second row writes");
        emitter
            .emit(&mut bytes, "rows", &json!({ "i": 2 }))
            .expect("third row writes");
        assert_eq!(emitter.count(), 3);

        let mut cursor = Cursor::new(bytes);
        let rows = [
            read_frame(&mut cursor, MAX_FRAME_BYTES).expect("first row reads"),
            read_frame(&mut cursor, MAX_FRAME_BYTES).expect("second row reads"),
            read_frame(&mut cursor, MAX_FRAME_BYTES).expect("third row reads"),
        ];
        assert_eq!(
            rows.map(|frame| frame.message),
            [
                Message::DataRow(DataRow {
                    stream: "rows".to_owned(),
                    sequence: 0,
                    payload: raw_json(&json!({ "i": 0 })),
                }),
                Message::DataRow(DataRow {
                    stream: "warnings".to_owned(),
                    sequence: 0,
                    payload: raw_json(&json!({ "i": 1 })),
                }),
                Message::DataRow(DataRow {
                    stream: "rows".to_owned(),
                    sequence: 1,
                    payload: raw_json(&json!({ "i": 2 })),
                }),
            ],
        );
    }

    #[test]
    fn oversized_data_row_is_rejected_before_write() {
        let row = DataRow {
            stream: "rows".to_owned(),
            sequence: 0,
            payload: raw_json(&json!({ "blob": "x".repeat(MAX_FRAME_BYTES as usize) })),
        };
        let mut bytes = Vec::new();
        let err =
            write_frame(&mut bytes, Message::DataRow(row), MAX_FRAME_BYTES).expect_err("oversized frame is rejected");
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
        let err = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect_err("oversized frame is rejected");
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
    fn larger_cap_accepts_frame_rejected_under_default() {
        // A 2 MiB payload is rejected under MAX_FRAME_BYTES (1 MiB) but
        // accepted when the cap is raised—proving the cap parameter is the
        // only thing the codec consults.
        let raised = MAX_FRAME_BYTES.saturating_mul(8);
        let row = DataRow {
            stream: "rows".to_owned(),
            sequence: 0,
            payload: raw_json(&json!({ "blob": "x".repeat(2 * MAX_FRAME_BYTES as usize) })),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, Message::DataRow(row.clone()), raised).expect("oversize-under-default frame writes");
        let frame = read_frame(&mut Cursor::new(buf), raised).expect("oversize-under-default frame reads");
        assert_eq!(frame.message, Message::DataRow(row));
    }

    #[test]
    fn frame_cap_bounds_constants_are_consistent() {
        const { assert!(MIN_FRAME_BYTES <= MAX_FRAME_BYTES) };
        const { assert!(MAX_FRAME_BYTES <= MAX_FRAME_BYTES_HARD_CAP) };
    }

    #[test]
    fn malformed_frame_payload_is_protocol_error() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.push(b'{');
        let err = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect_err("malformed JSON is rejected");
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
        write_frame(
            &mut bytes,
            Message::Response(Response::RowsComplete { count: 2 }),
            MAX_FRAME_BYTES,
        )
        .expect("rows complete writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("rows complete reads");
        assert_eq!(frame.message, Message::Response(Response::RowsComplete { count: 2 }));
    }

    #[test]
    fn declaration_search_request_and_response_round_trip() {
        let request = Message::Request(Request::SearchDeclarations {
            search: LeanWorkerDeclarationSearch {
                name_fragment: Some("map".to_owned()),
                name_match: LeanWorkerDeclarationNameMatch::Suffix,
                kind: Some("theorem".to_owned()),
                required_constants: vec!["List.map".to_owned()],
                conclusion_head: Some("Eq".to_owned()),
                scope_biases: vec![LeanWorkerDeclarationSearchBias {
                    scope: LeanWorkerDeclarationSearchScope::Namespace,
                    prefix: "List".to_owned(),
                    strict: false,
                    weight: 7,
                }],
                limit: 3,
                filter: LeanWorkerDeclarationFilter {
                    include_private: false,
                    include_generated: false,
                    include_internal: false,
                },
                include_source: false,
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, request.clone(), MAX_FRAME_BYTES).expect("declaration search request writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("declaration search request reads");
        assert_eq!(frame.message, request);

        let response = Message::Response(Response::DeclarationSearch {
            result: LeanWorkerDeclarationSearchResult {
                declarations: vec![LeanWorkerDeclarationSearchRow {
                    name: "List.map_map".to_owned(),
                    kind: "theorem".to_owned(),
                    module: Some("Init.Data.List.Lemmas".to_owned()),
                    source: None,
                    match_reason: "name,kind,required_constants,conclusion_head".to_owned(),
                    score: 127,
                    rank: 1,
                    flags: LeanWorkerDeclarationFlags::default(),
                }],
                truncated: true,
                facts: LeanWorkerDeclarationSearchFacts {
                    declarations_scanned: 100,
                    after_name_filter: 10,
                    after_kind_filter: 8,
                    after_required_constants_filter: 4,
                    after_conclusion_filter: 2,
                    after_scope_filter: 2,
                    source_lookups: 0,
                    broad_pruning: vec![LeanWorkerDeclarationSearchPruning {
                        stage: "limit".to_owned(),
                        reason: "broad_search_limit".to_owned(),
                        count: 1,
                    }],
                    truncated: true,
                    timings: LeanWorkerDeclarationSearchTimings {
                        scan_micros: 1000,
                        rank_micros: 50,
                        source_micros: 0,
                    },
                    derived_work: LeanWorkerDerivedWorkFacts::default(),
                },
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, response.clone(), MAX_FRAME_BYTES).expect("declaration search response writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("declaration search response reads");
        assert_eq!(frame.message, response);
    }

    #[test]
    fn declaration_inspection_request_and_response_round_trip() {
        let request = Message::Request(Request::InspectDeclaration {
            request: LeanWorkerDeclarationInspectionRequest {
                name: "List.map_map".to_owned(),
                fields: LeanWorkerDeclarationInspectionFields {
                    source: true,
                    statement: true,
                    docstring: true,
                    attributes: true,
                    flags: true,
                    rendering: LeanWorkerRendering::Pretty,
                    proof_search: true,
                },
                budgets: LeanWorkerOutputBudgets {
                    per_field_bytes: 128,
                    total_bytes: 512,
                },
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, request.clone(), MAX_FRAME_BYTES).expect("declaration inspection request writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("declaration inspection request reads");
        assert_eq!(frame.message, request);

        let response = Message::Response(Response::DeclarationInspection {
            result: LeanWorkerDeclarationInspectionResult::Found {
                declaration: Box::new(LeanWorkerDeclarationInspection {
                    name: "List.map_map".to_owned(),
                    kind: "theorem".to_owned(),
                    module: Some("Init.Data.List.Lemmas".to_owned()),
                    source: Some(LeanWorkerSourceRange {
                        file: "Init/Data/List/Lemmas.lean".to_owned(),
                        start_line: 1,
                        start_column: 1,
                        end_line: 1,
                        end_column: 10,
                    }),
                    statement: Some(LeanWorkerRenderedInfo {
                        value: "forall ...".to_owned(),
                        truncated: true,
                    }),
                    docstring: Some(LeanWorkerRenderedInfo {
                        value: "doc".to_owned(),
                        truncated: false,
                    }),
                    attributes: vec!["simp".to_owned(), "rw".to_owned()],
                    proof_search: LeanWorkerDeclarationProofSearchFacts {
                        computed: true,
                        unavailable_reason: None,
                        is_simp: true,
                        is_rw_candidate: true,
                        is_instance: false,
                        is_class: false,
                        class_name: None,
                    },
                    flags: LeanWorkerDeclarationFlags::default(),
                    derived_work: LeanWorkerDerivedWorkFacts::default(),
                    statement_rendering: Some(LeanWorkerRendering::Pretty),
                }),
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, response.clone(), MAX_FRAME_BYTES).expect("declaration inspection response writes");
        let frame =
            read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("declaration inspection response reads");
        assert_eq!(frame.message, response);

        let not_found = Message::Response(Response::DeclarationInspection {
            result: LeanWorkerDeclarationInspectionResult::NotFound {
                name: "Missing.name".to_owned(),
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, not_found.clone(), MAX_FRAME_BYTES)
            .expect("declaration inspection not-found response writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES)
            .expect("declaration inspection not-found response reads");
        assert_eq!(frame.message, not_found);

        let unsupported = Message::Response(Response::DeclarationInspection {
            result: LeanWorkerDeclarationInspectionResult::Unsupported,
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, unsupported.clone(), MAX_FRAME_BYTES)
            .expect("declaration inspection unsupported response writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES)
            .expect("declaration inspection unsupported response reads");
        assert_eq!(frame.message, unsupported);
    }

    #[test]
    fn module_query_request_and_response_round_trip() {
        let request = Message::Request(Request::ProcessModuleQuery {
            source: "def x := 1\n#check x\n".to_owned(),
            query: LeanWorkerModuleQuery::TypeAt { line: 2, column: 8 },
            options: LeanWorkerElabOptions::default(),
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, request.clone(), MAX_FRAME_BYTES).expect("module query request writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("module query request reads");
        assert_eq!(frame.message, request);

        let response = Message::Response(Response::ProcessModuleQuery {
            outcome: LeanWorkerModuleQueryOutcome::Ok {
                imports: Vec::new(),
                result: LeanWorkerModuleQueryResult::TypeAt(LeanWorkerTypeAtResult::Term {
                    span: LeanWorkerModuleSourceSpan {
                        start_line: 2,
                        start_column: 8,
                        end_line: 2,
                        end_column: 9,
                    },
                    expr: LeanWorkerRenderedInfo {
                        value: "x".to_owned(),
                        truncated: false,
                    },
                    type_str: LeanWorkerRenderedInfo {
                        value: "Nat".to_owned(),
                        truncated: false,
                    },
                    expected_type: None,
                }),
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, response.clone(), MAX_FRAME_BYTES).expect("module query response writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("module query response reads");
        assert_eq!(frame.message, response);

        let unsupported = LeanWorkerModuleQueryOutcome::Unsupported;
        let json = serde_json::to_value(&unsupported).expect("unsupported serializes");
        assert_eq!(json, json!({ "status": "unsupported" }));

        let diagnostics = LeanWorkerModuleQueryResult::Diagnostics(LeanWorkerElabFailure {
            diagnostics: Vec::new(),
            truncated: false,
        });
        let json = serde_json::to_value(&diagnostics).expect("diagnostics serializes");
        assert_eq!(
            json,
            json!({
                "result": "diagnostics",
                "body": {
                    "diagnostics": [],
                    "truncated": false
                }
            })
        );
    }

    #[test]
    fn module_query_batch_request_and_response_round_trip() {
        let request = Message::Request(Request::ProcessModuleQueryBatch {
            source: "theorem t : True := by\n  trivial\n".to_owned(),
            selectors: vec![
                LeanWorkerModuleQuerySelector::Diagnostics {
                    id: "diagnostics".to_owned(),
                },
                LeanWorkerModuleQuerySelector::ProofState {
                    id: "state".to_owned(),
                    line: 2,
                    column: 4,
                },
            ],
            budgets: LeanWorkerOutputBudgets::default(),
            options: LeanWorkerElabOptions::default(),
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, request.clone(), MAX_FRAME_BYTES).expect("module query batch request writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("module query batch request reads");
        assert_eq!(frame.message, request);

        let response = Message::Response(Response::ProcessModuleQueryBatch {
            outcome: LeanWorkerModuleQueryBatchOutcome::Ok {
                imports: Vec::new(),
                result: LeanWorkerModuleQueryBatchEnvelope {
                    items: vec![LeanWorkerModuleQueryBatchItem::Ok {
                        id: "diagnostics".to_owned(),
                        result: Box::new(LeanWorkerModuleQueryBatchResult::Diagnostics(LeanWorkerElabFailure {
                            diagnostics: Vec::new(),
                            truncated: false,
                        })),
                    }],
                    total_truncated: false,
                },
                facts: LeanWorkerModuleQueryCacheFacts {
                    cache_status: LeanWorkerModuleCacheStatus::Miss,
                    timings: LeanWorkerModuleQueryTimings::zero(),
                    output_bytes: 0,
                    cache_entry_count: Some(1),
                    cache_approx_bytes: Some(1024),
                    resource: None,
                },
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, response.clone(), MAX_FRAME_BYTES).expect("module query batch response writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("module query batch response reads");
        assert_eq!(frame.message, response);
    }

    #[test]
    fn module_query_cache_facts_resource_is_additive_wire_field() {
        let old_json = serde_json::json!({
            "cache_status": "miss",
            "timings": {
                "header_import_micros": 0,
                "elaboration_micros": 0,
                "projection_micros": 0,
                "rendering_micros": 0
            },
            "output_bytes": 0,
            "cache_entry_count": null,
            "cache_approx_bytes": null
        });
        let old_facts: LeanWorkerModuleQueryCacheFacts =
            serde_json::from_value(old_json).expect("old cache facts deserialize without resource");
        assert!(old_facts.resource.is_none());

        let resource = LeanWorkerResourceExhaustedFacts {
            cause: "worker_rss_hard_limit".to_owned(),
            work_entered_child: true,
            operation: Some("worker_process_module_query_batch".to_owned()),
            current_rss_kib: Some(2048),
            limit_kib: Some(1024),
            import_count: Some(1),
            worker_generation: Some(2),
            restart_reason: Some("rss_hard_limit".to_owned()),
            queue_wait_ms: None,
            duration_ms: None,
            cold_open_attempts: None,
            cold_open_admitted: None,
            cold_open_refusals: None,
            import_like_requests: Some(1),
            import_like_admitted: Some(1),
            last_import_stats: None,
        };
        let facts = LeanWorkerModuleQueryCacheFacts {
            resource: Some(Box::new(resource.clone())),
            ..LeanWorkerModuleQueryCacheFacts::uncached(0)
        };
        let round_trip: LeanWorkerModuleQueryCacheFacts =
            serde_json::from_value(serde_json::to_value(&facts).expect("facts serialize")).expect("facts deserialize");
        assert_eq!(round_trip.resource.as_deref(), Some(&resource));
    }

    #[test]
    fn proof_attempt_request_and_response_round_trip() {
        let span = LeanWorkerModuleSourceSpan {
            start_line: 1,
            start_column: 22,
            end_line: 2,
            end_column: 7,
        };
        let request = Message::Request(Request::AttemptProof {
            request: LeanWorkerProofAttemptRequest {
                source: "theorem t : True := by\n  trivial\n".to_owned(),
                edit: LeanWorkerProofEditTarget::Declaration {
                    name: "t".to_owned(),
                    position: LeanWorkerProofPositionSelector::Default,
                },
                candidates: vec![LeanWorkerProofCandidate {
                    id: "rfl".to_owned(),
                    text: "by trivial".to_owned(),
                }],
                budgets: LeanWorkerOutputBudgets::default(),
            },
            options: LeanWorkerElabOptions::default(),
            progress: true,
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, request.clone(), MAX_FRAME_BYTES).expect("proof attempt request writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("proof attempt request reads");
        assert_eq!(frame.message, request);

        let response = Message::Response(Response::ProofAttempt {
            result: LeanWorkerProofAttemptResult::Ok {
                imports: Vec::new(),
                result: LeanWorkerProofAttemptEnvelope {
                    candidates: vec![LeanWorkerProofAttemptRow {
                        id: "rfl".to_owned(),
                        status: LeanWorkerProofAttemptStatus::Closed,
                        candidate_text: LeanWorkerRenderedInfo {
                            value: "rfl".to_owned(),
                            truncated: false,
                        },
                        diagnostics: LeanWorkerElabFailure {
                            diagnostics: Vec::new(),
                            truncated: false,
                        },
                        downstream_diagnostics: LeanWorkerElabFailure {
                            diagnostics: Vec::new(),
                            truncated: false,
                        },
                        goals: Vec::new(),
                        declaration: Some(LeanWorkerDeclarationTargetInfo {
                            short_name: "t".to_owned(),
                            declaration_name: "t".to_owned(),
                            namespace_name: String::new(),
                            declaration_kind: "theorem".to_owned(),
                            declaration_span: span.clone(),
                            name_span: span.clone(),
                            body_span: span,
                        }),
                        proof_position: Some(LeanWorkerProofPositionSummary {
                            index: 0,
                            tactic: LeanWorkerRenderedInfo {
                                value: "trivial".to_owned(),
                                truncated: false,
                            },
                        }),
                        output_truncated: false,
                    }],
                    candidate_limit: 8,
                    candidates_truncated: false,
                },
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, response.clone(), MAX_FRAME_BYTES).expect("proof attempt response writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("proof attempt response reads");
        assert_eq!(frame.message, response);
    }

    #[test]
    fn proof_position_selector_tags_are_stable_and_round_trip() {
        let cases = [
            (
                LeanWorkerProofPositionSelector::Default,
                serde_json::json!({"kind": "default"}),
            ),
            (
                LeanWorkerProofPositionSelector::Index { index: 3 },
                serde_json::json!({"kind": "index", "index": 3}),
            ),
            (
                LeanWorkerProofPositionSelector::AfterText {
                    text: "intro x".to_owned(),
                    occurrence: Some(1),
                },
                serde_json::json!({"kind": "after_text", "text": "intro x", "occurrence": 1}),
            ),
            (
                LeanWorkerProofPositionSelector::Entry,
                serde_json::json!({"kind": "entry"}),
            ),
        ];
        for (selector, expected) in cases {
            let value = serde_json::to_value(&selector).expect("selector serializes");
            assert_eq!(value, expected, "selector tag must be stable: {selector:?}");
            let parsed: LeanWorkerProofPositionSelector =
                serde_json::from_value(expected).expect("selector deserializes");
            assert_eq!(parsed, selector, "selector must round-trip through JSON");
        }
    }

    #[test]
    fn declaration_verification_request_and_response_round_trip() {
        let request = Message::Request(Request::VerifyDeclaration {
            request: LeanWorkerDeclarationVerificationRequest {
                source: "theorem t : True := by\n  trivial\n".to_owned(),
                target: LeanWorkerDeclarationVerificationTarget::Name { name: "t".to_owned() },
                sorry_policy: LeanWorkerSorryPolicy::Deny,
                report_axioms: true,
                budgets: LeanWorkerOutputBudgets::default(),
            },
            options: LeanWorkerElabOptions::default(),
            progress: false,
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, request.clone(), MAX_FRAME_BYTES).expect("verification request writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("verification request reads");
        assert_eq!(frame.message, request);

        let response = Message::Response(Response::DeclarationVerification {
            result: LeanWorkerDeclarationVerificationResult::Ok {
                verification_status: LeanWorkerDeclarationVerificationStatus::Accepted,
                facts: Box::new(LeanWorkerDeclarationVerificationFacts {
                    target: None,
                    diagnostics: LeanWorkerElabFailure {
                        diagnostics: Vec::new(),
                        truncated: false,
                    },
                    unresolved_goals: Vec::new(),
                    contains_sorry: false,
                    contains_admit: false,
                    contains_sorry_ax: false,
                    axioms: vec!["propext".to_owned(), "Classical.choice".to_owned()],
                    axioms_truncated: false,
                    output_truncated: false,
                    candidates: Vec::new(),
                    axioms_available: true,
                }),
                imports: Vec::new(),
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, response.clone(), MAX_FRAME_BYTES).expect("verification response writes");
        let frame = read_frame(&mut Cursor::new(bytes), MAX_FRAME_BYTES).expect("verification response reads");
        assert_eq!(frame.message, response);
    }

    #[test]
    fn verification_needs_build_and_ambiguous_round_trip() {
        // NeedsBuild verdict: the enclosing MissingImports outcome names the
        // unbuilt modules; the status is the typed resolution verdict.
        let needs_build = Message::Response(Response::DeclarationVerification {
            result: LeanWorkerDeclarationVerificationResult::MissingImports {
                verification_status: LeanWorkerDeclarationVerificationStatus::NeedsBuild,
                facts: Box::new(verification_facts_fixture(Vec::new(), false)),
                imports: vec!["Mathlib.Tactic".to_owned()],
                missing: vec!["Mathlib.Unbuilt.Dep".to_owned()],
            },
        });
        assert_frame_round_trips(&needs_build);

        // Ambiguous verdict carries the competing declarations.
        let ambiguous = Message::Response(Response::DeclarationVerification {
            result: LeanWorkerDeclarationVerificationResult::Ok {
                verification_status: LeanWorkerDeclarationVerificationStatus::Ambiguous,
                facts: Box::new(verification_facts_fixture(
                    vec![
                        declaration_target_info_fixture("A.foo"),
                        declaration_target_info_fixture("B.foo"),
                    ],
                    false,
                )),
                imports: Vec::new(),
            },
        });
        assert_frame_round_trips(&ambiguous);
    }

    #[test]
    fn proof_state_ambiguous_and_needs_build_round_trip() {
        let ambiguous = Message::Response(Response::ProcessModuleQueryBatch {
            outcome: LeanWorkerModuleQueryBatchOutcome::Ok {
                result: LeanWorkerModuleQueryBatchEnvelope {
                    items: vec![LeanWorkerModuleQueryBatchItem::Ok {
                        id: "state".to_owned(),
                        result: Box::new(LeanWorkerModuleQueryBatchResult::ProofState(
                            LeanWorkerProofStateResult::Ambiguous {
                                candidates: vec![
                                    declaration_target_info_fixture("A.foo"),
                                    declaration_target_info_fixture("B.foo"),
                                ],
                            },
                        )),
                    }],
                    total_truncated: false,
                },
                imports: Vec::new(),
                facts: LeanWorkerModuleQueryCacheFacts::uncached(0),
            },
        });
        assert_frame_round_trips(&ambiguous);

        let needs_build = Message::Response(Response::ProcessModuleQueryBatch {
            outcome: LeanWorkerModuleQueryBatchOutcome::Ok {
                result: LeanWorkerModuleQueryBatchEnvelope {
                    items: vec![LeanWorkerModuleQueryBatchItem::Ok {
                        id: "state".to_owned(),
                        result: Box::new(LeanWorkerModuleQueryBatchResult::ProofState(
                            LeanWorkerProofStateResult::NeedsBuild {
                                missing: vec!["Mathlib.Unbuilt.Dep".to_owned()],
                            },
                        )),
                    }],
                    total_truncated: false,
                },
                imports: Vec::new(),
                facts: LeanWorkerModuleQueryCacheFacts::uncached(0),
            },
        });
        assert_frame_round_trips(&needs_build);
    }

    #[test]
    fn inspection_fields_default_rendering_is_pretty() {
        // A request serialized without an explicit `rendering` deserializes to
        // Pretty (the `#[serde(default)]`), so older callers get the useful
        // notation-aware form by default.
        let json = serde_json::json!({
            "source": true,
            "statement": true,
            "docstring": false,
            "attributes": false,
            "flags": false,
        });
        let fields: LeanWorkerDeclarationInspectionFields =
            serde_json::from_value(json).expect("fields without rendering deserialize");
        assert_eq!(fields.rendering, LeanWorkerRendering::Pretty);
        assert!(!fields.proof_search);
    }
}
