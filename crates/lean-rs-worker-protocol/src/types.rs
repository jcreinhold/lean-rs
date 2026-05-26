//! Wire-stable, serde-derived value types crossing the worker process boundary.
//!
//! These types are the single representation of every shape that flows through
//! the worker IPC. The earlier triple-layer split (host type → `pub(crate)`
//! wire type → public worker mirror) collapsed once it became clear that the
//! worker's "different abstraction" from the host is process supervision —
//! not data shape — so the wire format and the public surface are the same
//! concern. See `docs/architecture/16-production-boundary.md` for the boundary
//! contract.
//!
//! Conversion from opaque host types (`LeanExpr`, `LeanName`, …) into these
//! value types lives in the worker child runtime next to the Lean calls that
//! produce them. No type in this module references `lean_rs_host`; the
//! asymmetry is deliberate so the worker's public API does not couple to
//! host's semver.
//!
//! ## Additive evolution
//!
//! Every public enum carries `#[non_exhaustive]` so a new variant is additive
//! and consumers must include a wildcard match arm. Structs in this module
//! keep their fields `pub` and are not `#[non_exhaustive]`: they are JSON
//! payloads whose shape is fixed by the wire contract, so adding a field is
//! already a breaking wire change regardless of Rust-side annotations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bounded elaboration options for worker-session requests.
///
/// Mirrors the stable knobs from `lean_rs_host::LeanElabOptions` without
/// exposing the in-child host object across the process boundary. The child
/// applies the host ceilings for `heartbeat_limit` and `diagnostic_byte_limit`;
/// the values here are caller intent, not post-clamp guarantees.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerElabOptions {
    pub namespace_context: String,
    pub file_label: String,
    pub heartbeat_limit: u64,
    pub diagnostic_byte_limit: usize,
}

impl LeanWorkerElabOptions {
    /// Create worker elaboration options with host defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the namespace context.
    #[must_use]
    pub fn namespace_context(mut self, namespace: &str) -> Self {
        namespace.clone_into(&mut self.namespace_context);
        self
    }

    /// Replace the diagnostic file label.
    #[must_use]
    pub fn file_label(mut self, label: &str) -> Self {
        label.clone_into(&mut self.file_label);
        self
    }

    /// Replace the heartbeat limit. The child applies the host ceiling.
    #[must_use]
    pub fn heartbeat_limit(mut self, heartbeats: u64) -> Self {
        self.heartbeat_limit = heartbeats;
        self
    }

    /// Replace the diagnostic byte limit. The child applies the host ceiling.
    #[must_use]
    pub fn diagnostic_byte_limit(mut self, bytes: usize) -> Self {
        self.diagnostic_byte_limit = bytes;
        self
    }
}

impl Default for LeanWorkerElabOptions {
    fn default() -> Self {
        Self {
            namespace_context: String::new(),
            file_label: "<elaborate>".to_owned(),
            heartbeat_limit: lean_toolchain::LEAN_HEARTBEAT_LIMIT_DEFAULT,
            diagnostic_byte_limit: lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT,
        }
    }
}

/// Serializable elaboration result returned over the worker boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerElabResult {
    pub success: bool,
    pub diagnostics: Vec<LeanWorkerDiagnostic>,
    pub truncated: bool,
}

/// Kernel-check status returned over the worker boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerKernelStatus {
    Checked,
    Rejected,
    Unavailable,
    Unsupported,
}

/// Serializable kernel-check result returned over the worker boundary.
///
/// `summary` is `Some` if and only if `status == Checked`; the field is
/// populated from `lean_rs_host::LeanSession::summarize_evidence` against the
/// proof evidence the kernel returned. The three failure statuses leave it
/// `None`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerKernelResult {
    pub status: LeanWorkerKernelStatus,
    pub diagnostics: Vec<LeanWorkerDiagnostic>,
    pub truncated: bool,
    pub summary: Option<LeanWorkerKernelSummary>,
}

/// Projection of `lean_rs_host::ProofSummary` for the kernel-check success arm.
///
/// `declaration_name` is a dotted-path rendering of the checked declaration
/// (diagnostic only — multiple distinct `Lean.Name`s can render to the same
/// string). `kind` is one of `"theorem"`, `"definition"`, `"axiom"`,
/// `"opaque"`, or `"unsupported"`. `type_signature` is the pretty-printed
/// declaration type as the host's `ProofSummary` emits it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerKernelSummary {
    pub declaration_name: String,
    pub kind: String,
    pub type_signature: String,
}

/// One diagnostic emitted by worker elaboration, kernel checks, or `MetaM`
/// services.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDiagnostic {
    pub severity: String,
    pub message: String,
    pub file_label: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub end_line: Option<u32>,
    pub end_column: Option<u32>,
}

/// Diagnostic payload returned alongside meta and elaboration failures.
///
/// `truncated` indicates the diagnostic projection hit the host byte budget
/// and later messages were dropped.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerElabFailure {
    pub diagnostics: Vec<LeanWorkerDiagnostic>,
    pub truncated: bool,
}

/// Reducibility setting for `is_def_eq`, mirroring
/// `lean_rs_host::meta::LeanMetaTransparency`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerMetaTransparency {
    /// Lean's standard reducibility (default).
    #[default]
    Default,
    /// Only `@[reducible]` definitions unfold.
    Reducible,
    /// Default plus instance-binding bodies.
    Instances,
    /// Every definition unfolds (most aggressive).
    All,
}

/// A Lean expression rendered to a string, together with the rendering path
/// that produced it.
///
/// `LeanWorkerSession::infer_type` and `whnf` attempt notation-aware rendering
/// via the optional `meta_pp_expr` shim and fall back to `Expr.toString` when
/// the shim is absent or reports `Unsupported`. The `rendering` field reports
/// which path produced the `value`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerRendered {
    pub value: String,
    pub rendering: LeanWorkerRendering,
}

/// Which rendering path produced a [`LeanWorkerRendered::value`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerRendering {
    /// Rendered via `Lean.PrettyPrinter.ppExpr` (notation-aware).
    Pretty,
    /// Rendered via `Expr.toString` (deterministic, no notation). Either the
    /// `meta_pp_expr` shim was absent on the loaded capability, or the
    /// pretty-printer reported `Unsupported`.
    Raw,
}

/// Outcome of one bounded `MetaM` service call over the worker boundary.
///
/// Mirrors `lean_rs_host::meta::LeanMetaResponse<T>`. Callers branch on the
/// variant; the typed payload lives in `Ok { value }`, and the three
/// non-success arms carry a structured [`LeanWorkerElabFailure`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerMetaResult<T> {
    /// The `MetaM` action returned a typed payload.
    Ok { value: T },
    /// The `MetaM` action raised a non-resource-exhaustion exception.
    Failed { failure: LeanWorkerElabFailure },
    /// The heartbeat ceiling tripped before the action finished.
    TimeoutOrHeartbeat { failure: LeanWorkerElabFailure },
    /// The capability did not provide this service.
    Unsupported { failure: LeanWorkerElabFailure },
}

/// Filter applied when enumerating declarations from a session's open
/// environment.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationFilter {
    /// Keep names Lean marks as private.
    pub include_private: bool,
    /// Keep generated names with numeric components.
    pub include_generated: bool,
    /// Keep Lean internal-detail names such as `_`, `match_`, `proof_`, ….
    pub include_internal: bool,
}

impl Default for LeanWorkerDeclarationFilter {
    fn default() -> Self {
        Self {
            include_private: true,
            include_generated: false,
            include_internal: false,
        }
    }
}

/// Source range Lean recorded for one declaration. Positions are 1-based.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerSourceRange {
    pub file: String,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// One declaration row returned by `LeanWorkerSession::describe` or
/// `LeanWorkerSession::describe_bulk`.
///
/// `kind` is the literal string `LeanSession::declaration_kind` returns
/// (`"axiom"`, `"definition"`, `"theorem"`, …, or `"missing"` for an absent
/// name). The `describe_bulk` path preserves the slot for absent names by
/// keeping `kind == "missing"` with `type_signature: None` and `source: None`
/// so the response length matches the input length.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationRow {
    pub name: String,
    pub kind: String,
    pub type_signature: Option<String>,
    pub source: Option<LeanWorkerSourceRange>,
}

/// One identifier occurrence the elaborator recorded. `is_binder` distinguishes
/// binding sites from use sites.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerNameRef {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub name: String,
    pub is_binder: bool,
}

/// Query shape for one header-aware Lean module processing request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "query", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerModuleQuery {
    Diagnostics,
    TypeAt { line: u32, column: u32 },
    GoalAt { line: u32, column: u32 },
    References { name: String },
}

/// Source span in the original file. Positions are 1-based.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerModuleSourceSpan {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// Bounded rendered Lean text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerRenderedInfo {
    pub value: String,
    pub truncated: bool,
}

/// Result for `LeanWorkerModuleQuery::TypeAt`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerTypeAtResult {
    Term {
        span: LeanWorkerModuleSourceSpan,
        expr: LeanWorkerRenderedInfo,
        type_str: LeanWorkerRenderedInfo,
        expected_type: Option<LeanWorkerRenderedInfo>,
    },
    NoTerm,
}

/// Result for `LeanWorkerModuleQuery::GoalAt`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerGoalAtResult {
    Goal {
        span: LeanWorkerModuleSourceSpan,
        goals_before: Vec<String>,
        goals_after: Vec<String>,
        truncated: bool,
    },
    NoTacticContext,
}

/// Result for `LeanWorkerModuleQuery::References`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerReferencesResult {
    pub references: Vec<LeanWorkerNameRef>,
    pub truncated: bool,
}

/// Typed payload returned by a successful module query.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", content = "body", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerModuleQueryResult {
    Diagnostics(LeanWorkerElabFailure),
    TypeAt(LeanWorkerTypeAtResult),
    GoalAt(LeanWorkerGoalAtResult),
    References(LeanWorkerReferencesResult),
}

/// Outcome of `LeanWorkerSession::process_module_query`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerModuleQueryOutcome {
    /// Header parsed; every parsed import is present in the session's open
    /// env; the query result is populated.
    Ok {
        result: LeanWorkerModuleQueryResult,
        imports: Vec<String>,
    },
    /// Header parsed but some imports name modules the session's open env
    /// does not have. The body was still queried against the available env.
    MissingImports {
        result: LeanWorkerModuleQueryResult,
        imports: Vec<String>,
        missing: Vec<String>,
    },
    /// `Lean.Parser.parseHeader` reported error-severity messages; the body
    /// was never elaborated.
    HeaderParseFailed { diagnostics: LeanWorkerElabFailure },
    /// The capability dylib does not export
    /// `lean_rs_host_process_module_query`.
    Unsupported,
}

/// Generic metadata reported by one downstream capability package.
///
/// Command names, capability names, versions, and `extra` JSON are downstream
/// semantics. `lean-rs-worker` transports and validates the envelope; it does
/// not decide which values affect caches.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerCapabilityMetadata {
    pub commands: Vec<LeanWorkerCommandMetadata>,
    pub capabilities: Vec<LeanWorkerCapabilityFact>,
    pub lean_version: Option<String>,
    pub extra: Option<Value>,
}

/// One downstream command advertised by capability metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerCommandMetadata {
    pub name: String,
    pub version: String,
}

/// One named capability advertised by capability metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerCapabilityFact {
    pub name: String,
    pub version: String,
}

/// Capability health report returned by a downstream doctor export.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDoctorReport {
    pub diagnostics: Vec<LeanWorkerDoctorDiagnostic>,
    pub metadata: Option<Value>,
}

/// One structured capability health diagnostic.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDoctorDiagnostic {
    pub severity: LeanWorkerDoctorSeverity,
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

/// Severity for a capability doctor diagnostic.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerDoctorSeverity {
    Pass,
    Warning,
    Error,
}
