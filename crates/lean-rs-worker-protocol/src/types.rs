//! Wire-stable, serde-derived value types crossing the worker process boundary.
//!
//! These types are the single representation of every shape that flows through
//! the worker IPC. The earlier triple-layer split (host type → `pub(crate)`
//! wire type → public worker mirror) collapsed once it became clear that the
//! worker's "different abstraction" from the host is process supervision—
//! not data shape—so the wire format and the public surface are the same
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
/// (diagnostic only—multiple distinct `Lean.Name`s can render to the same
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

/// Name-fragment matching policy for declaration search.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanWorkerDeclarationNameMatch {
    /// Case-insensitive substring match.
    #[default]
    Contains,
    /// Case-insensitive suffix match.
    Suffix,
}

/// Scope kind used by declaration-search ranking bias.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanWorkerDeclarationSearchScope {
    Namespace,
    Module,
}

/// Optional namespace/module preference for declaration search.
///
/// Non-strict biases affect ranking only. Strict biases act as filters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationSearchBias {
    pub scope: LeanWorkerDeclarationSearchScope,
    pub prefix: String,
    pub strict: bool,
    pub weight: i32,
}

/// Bounded structured declaration search request.
///
/// Search inspects declaration metadata and Lean expressions structurally, but
/// never renders declaration type text. Use the worker session's explicit
/// one-name declaration-type query for type rendering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationSearch {
    pub name_fragment: Option<String>,
    pub name_match: LeanWorkerDeclarationNameMatch,
    pub kind: Option<String>,
    pub required_constants: Vec<String>,
    pub conclusion_head: Option<String>,
    pub scope_biases: Vec<LeanWorkerDeclarationSearchBias>,
    pub limit: usize,
    pub filter: LeanWorkerDeclarationFilter,
    pub include_source: bool,
}

impl LeanWorkerDeclarationSearch {
    /// Build a bounded structured declaration search request.
    #[must_use]
    pub fn new(name_fragment: impl Into<String>) -> Self {
        Self {
            name_fragment: Some(name_fragment.into()),
            name_match: LeanWorkerDeclarationNameMatch::Contains,
            kind: None,
            required_constants: Vec::new(),
            conclusion_head: None,
            scope_biases: Vec::new(),
            limit: 20,
            filter: LeanWorkerDeclarationFilter {
                include_private: false,
                include_generated: false,
                include_internal: false,
            },
            include_source: true,
        }
    }
}

/// Compact declaration flags returned by declaration search.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationFlags {
    pub is_private: bool,
    pub is_generated: bool,
    pub is_internal: bool,
}

/// One bounded metadata row returned by declaration search.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationSearchRow {
    pub name: String,
    pub kind: String,
    pub module: Option<String>,
    pub source: Option<LeanWorkerSourceRange>,
    pub match_reason: String,
    pub score: i32,
    pub rank: usize,
    pub flags: LeanWorkerDeclarationFlags,
}

/// One broad fanout pruning record returned in search facts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationSearchPruning {
    pub stage: String,
    pub reason: String,
    pub count: usize,
}

/// Cheap elapsed timings for declaration search.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationSearchTimings {
    pub scan_micros: u64,
    pub rank_micros: u64,
    pub source_micros: u64,
}

/// Fanout and timing facts for a bounded declaration search.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationSearchFacts {
    pub declarations_scanned: usize,
    pub after_name_filter: usize,
    pub after_kind_filter: usize,
    pub after_required_constants_filter: usize,
    pub after_conclusion_filter: usize,
    pub after_scope_filter: usize,
    pub source_lookups: usize,
    pub broad_pruning: Vec<LeanWorkerDeclarationSearchPruning>,
    pub truncated: bool,
    pub timings: LeanWorkerDeclarationSearchTimings,
}

/// Result of a bounded declaration search.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationSearchResult {
    pub declarations: Vec<LeanWorkerDeclarationSearchRow>,
    pub truncated: bool,
    pub facts: LeanWorkerDeclarationSearchFacts,
}

/// Bounded type rendering for a single declaration.
///
/// `type_signature`, when present, is capped by the request's byte limit and
/// marked `truncated` when the rendered Lean expression did not fit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationType {
    pub name: String,
    pub kind: String,
    pub type_signature: Option<LeanWorkerRenderedInfo>,
    pub source: Option<LeanWorkerSourceRange>,
}

/// Field selection for one declaration inspection request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "wire field-selection flags mirror the Lean request shape and are clearer than five tiny enums"
)]
pub struct LeanWorkerDeclarationInspectionFields {
    pub source: bool,
    pub statement: bool,
    pub docstring: bool,
    pub attributes: bool,
    pub flags: bool,
    /// How to render `statement`. Defaults to
    /// [`LeanWorkerRendering::Pretty`] (notation-aware, `pp.universes false`),
    /// which falls back to [`LeanWorkerRendering::Raw`] when the pretty-printer
    /// cannot render the term. Request `Raw` for the fully-elaborated form.
    #[serde(default = "rendering_pretty")]
    pub rendering: LeanWorkerRendering,
}

fn rendering_pretty() -> LeanWorkerRendering {
    LeanWorkerRendering::Pretty
}

impl Default for LeanWorkerDeclarationInspectionFields {
    fn default() -> Self {
        Self {
            source: true,
            statement: true,
            docstring: true,
            attributes: true,
            flags: true,
            rendering: LeanWorkerRendering::Pretty,
        }
    }
}

/// Bounded request to inspect one selected declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationInspectionRequest {
    pub name: String,
    pub fields: LeanWorkerDeclarationInspectionFields,
    pub budgets: LeanWorkerOutputBudgets,
}

impl LeanWorkerDeclarationInspectionRequest {
    /// Inspect one declaration with the default field set and output budgets.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: LeanWorkerDeclarationInspectionFields::default(),
            budgets: LeanWorkerOutputBudgets::default(),
        }
    }
}

/// Cheap proof-search facts for one declaration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "proof-search booleans are independent wire facts, not control-flow state"
)]
pub struct LeanWorkerDeclarationProofSearchFacts {
    pub is_simp: bool,
    pub is_rw_candidate: bool,
    pub is_instance: bool,
    pub is_class: bool,
    pub class_name: Option<String>,
}

/// Bounded facts for one inspected declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationInspection {
    pub name: String,
    pub kind: String,
    pub module: Option<String>,
    pub source: Option<LeanWorkerSourceRange>,
    pub statement: Option<LeanWorkerRenderedInfo>,
    pub docstring: Option<LeanWorkerRenderedInfo>,
    pub attributes: Vec<String>,
    pub proof_search: LeanWorkerDeclarationProofSearchFacts,
    pub flags: LeanWorkerDeclarationFlags,
    /// Rendering that actually produced `statement`: `Some(Pretty)` or
    /// `Some(Raw)`, or `None` when no statement was requested. Lets the caller
    /// tell whether the pretty path fired or fell back to the raw term.
    #[serde(default)]
    pub statement_rendering: Option<LeanWorkerRendering>,
}

/// Outcome of inspecting one selected declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerDeclarationInspectionResult {
    Found {
        declaration: Box<LeanWorkerDeclarationInspection>,
    },
    NotFound {
        name: String,
    },
    Unsupported,
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

/// Explicit byte budgets for batched module projections.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerOutputBudgets {
    pub per_field_bytes: u32,
    pub total_bytes: u32,
}

impl Default for LeanWorkerOutputBudgets {
    fn default() -> Self {
        Self {
            per_field_bytes: 8 * 1024,
            total_bytes: 64 * 1024,
        }
    }
}

/// One selector in a batched module-processing request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "selector", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerModuleQuerySelector {
    Diagnostics {
        id: String,
    },
    ProofState {
        id: String,
        line: u32,
        column: u32,
    },
    ProofStateInDeclaration {
        id: String,
        declaration: String,
        position: LeanWorkerProofPositionSelector,
        /// Render local hypotheses as raw, fully-elaborated `Expr` text instead
        /// of the default notation-aware delaboration. Defaults to `false`, so
        /// older callers that omit the field get the pretty rendering.
        #[serde(default)]
        locals_raw: bool,
    },
    TypeAt {
        id: String,
        line: u32,
        column: u32,
    },
    References {
        id: String,
        name: String,
    },
    DeclarationTarget {
        id: String,
        name: Option<String>,
        line: Option<u32>,
        column: Option<u32>,
    },
    SurroundingDeclaration {
        id: String,
        line: u32,
        column: u32,
    },
}

impl LeanWorkerModuleQuerySelector {
    /// The caller-chosen correlation id carried by every selector variant.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Diagnostics { id }
            | Self::ProofState { id, .. }
            | Self::ProofStateInDeclaration { id, .. }
            | Self::TypeAt { id, .. }
            | Self::References { id, .. }
            | Self::DeclarationTarget { id, .. }
            | Self::SurroundingDeclaration { id, .. } => id,
        }
    }
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

/// Intent selector for one proof position inside a declaration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerProofPositionSelector {
    /// Select the first tactic state in declaration order.
    #[default]
    Default,
    /// Select the `index`-th tactic state in declaration order.
    Index { index: u32 },
    /// Select the tactic whose source text exactly matches `text`.
    AfterText {
        text: String,
        #[serde(default)]
        occurrence: Option<u32>,
    },
}

/// Target for a non-mutating proof attempt over an in-memory source overlay.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerProofEditTarget {
    /// Try a tactic fragment at a selected proof position inside one declaration.
    Declaration {
        name: String,
        #[serde(default)]
        position: LeanWorkerProofPositionSelector,
    },
}

/// One proof candidate to apply to an in-memory source overlay.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerProofCandidate {
    pub id: String,
    pub text: String,
}

/// Bounded request to try one or more proof snippets without writing files.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerProofAttemptRequest {
    pub source: String,
    pub edit: LeanWorkerProofEditTarget,
    pub candidates: Vec<LeanWorkerProofCandidate>,
    pub budgets: LeanWorkerOutputBudgets,
}

/// Per-candidate proof attempt status.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerProofAttemptStatus {
    Closed,
    Progressed,
    Failed,
    Timeout,
    BudgetExceeded,
    Unsupported,
}

/// Per-candidate proof attempt result row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerProofAttemptRow {
    pub id: String,
    pub status: LeanWorkerProofAttemptStatus,
    pub candidate_text: LeanWorkerRenderedInfo,
    pub diagnostics: LeanWorkerElabFailure,
    pub downstream_diagnostics: LeanWorkerElabFailure,
    pub goals: Vec<LeanWorkerRenderedInfo>,
    pub declaration: Option<LeanWorkerDeclarationTargetInfo>,
    pub proof_position: Option<LeanWorkerProofPositionSummary>,
    pub output_truncated: bool,
}

/// Informational summary of the selected proof position. It is not an edit
/// handle and cannot be fed back into proof-action requests.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerProofPositionSummary {
    pub index: u32,
    pub tactic: LeanWorkerRenderedInfo,
}

/// Envelope for a bounded proof attempt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerProofAttemptEnvelope {
    pub candidates: Vec<LeanWorkerProofAttemptRow>,
    pub candidate_limit: u32,
    pub candidates_truncated: bool,
}

/// Header-aware proof attempt outcome.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerProofAttemptResult {
    Ok {
        result: LeanWorkerProofAttemptEnvelope,
        imports: Vec<String>,
    },
    MissingImports {
        result: LeanWorkerProofAttemptEnvelope,
        imports: Vec<String>,
        missing: Vec<String>,
    },
    HeaderParseFailed {
        diagnostics: LeanWorkerElabFailure,
    },
    Unsupported,
}

/// Target declaration for verification.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerDeclarationVerificationTarget {
    Name { name: String },
    Span { span: LeanWorkerModuleSourceSpan },
}

/// Policy for `sorry`-like constructs in declaration verification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerSorryPolicy {
    Allow,
    Deny,
}

/// Bounded request to verify one declaration in an in-memory source snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationVerificationRequest {
    pub source: String,
    pub target: LeanWorkerDeclarationVerificationTarget,
    pub sorry_policy: LeanWorkerSorryPolicy,
    pub report_axioms: bool,
    pub budgets: LeanWorkerOutputBudgets,
}

/// Verification policy result after diagnostics and declaration facts are
/// collected.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerDeclarationVerificationStatus {
    Accepted,
    Rejected,
    NotFound,
    Ambiguous,
    Timeout,
    BudgetExceeded,
    Unsupported,
    /// The name did not resolve because the open environment is incomplete.
    /// The enclosing
    /// [`LeanWorkerDeclarationVerificationResult::MissingImports`] names the
    /// unbuilt modules.
    NeedsBuild,
}

/// Bounded facts returned by declaration verification.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "verification booleans are independent wire facts for policy decisions"
)]
pub struct LeanWorkerDeclarationVerificationFacts {
    pub target: Option<LeanWorkerDeclarationTargetInfo>,
    pub diagnostics: LeanWorkerElabFailure,
    pub unresolved_goals: Vec<LeanWorkerRenderedInfo>,
    pub contains_sorry: bool,
    pub contains_admit: bool,
    pub contains_sorry_ax: bool,
    pub axioms: Vec<String>,
    pub axioms_truncated: bool,
    pub output_truncated: bool,
    /// Competing declarations when `verification_status` is `Ambiguous`; empty
    /// otherwise.
    #[serde(default)]
    pub candidates: Vec<LeanWorkerDeclarationTargetInfo>,
    /// `false` when the axiom dependency set could not be computed (the target
    /// did not resolve, or the walk was not requested): an empty `axioms` then
    /// means "not computed", not "no axioms". `true` with empty `axioms` is a
    /// genuine no-nontrivial-axioms result.
    #[serde(default)]
    pub axioms_available: bool,
}

impl LeanWorkerDeclarationVerificationFacts {
    /// Facts for a verdict the worker could not substantiate — for example when
    /// the child aborted mid-job and the supervisor synthesised a degraded
    /// verdict. Every field is empty and `axioms_available` is `false`, so the
    /// axiom set reads as "not computed" rather than "no axioms".
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
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
            candidates: Vec::new(),
            axioms_available: false,
        }
    }
}

/// Header-aware declaration verification outcome.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerDeclarationVerificationResult {
    Ok {
        verification_status: LeanWorkerDeclarationVerificationStatus,
        facts: Box<LeanWorkerDeclarationVerificationFacts>,
        imports: Vec<String>,
    },
    MissingImports {
        verification_status: LeanWorkerDeclarationVerificationStatus,
        facts: Box<LeanWorkerDeclarationVerificationFacts>,
        imports: Vec<String>,
        missing: Vec<String>,
    },
    HeaderParseFailed {
        diagnostics: LeanWorkerElabFailure,
    },
    Unsupported,
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

/// One local declaration in a proof-state result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerLocalInfo {
    pub name: String,
    pub binder_info: String,
    pub type_str: LeanWorkerRenderedInfo,
    pub value: Option<LeanWorkerRenderedInfo>,
}

/// Source metadata for the declaration surrounding a proof-agent query.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerDeclarationTargetInfo {
    pub short_name: String,
    pub declaration_name: String,
    pub namespace_name: String,
    pub declaration_kind: String,
    pub declaration_span: LeanWorkerModuleSourceSpan,
    pub name_span: LeanWorkerModuleSourceSpan,
    pub body_span: LeanWorkerModuleSourceSpan,
}

/// Result for `LeanWorkerModuleQuerySelector::DeclarationTarget`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerDeclarationTargetResult {
    Target {
        info: LeanWorkerDeclarationTargetInfo,
    },
    NotFound,
    Ambiguous {
        candidates: Vec<LeanWorkerDeclarationTargetInfo>,
    },
}

/// Proof-state payload for one cursor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerProofStateInfo {
    pub declaration_name: Option<String>,
    pub namespace_name: String,
    pub safe_edit: Option<LeanWorkerDeclarationTargetInfo>,
    pub span: LeanWorkerModuleSourceSpan,
    pub goals_before: Vec<String>,
    pub goals_after: Vec<String>,
    pub locals: Vec<LeanWorkerLocalInfo>,
    pub expected_type: Option<LeanWorkerRenderedInfo>,
    pub truncated: bool,
}

/// Result for `LeanWorkerModuleQuerySelector::ProofState`.
///
/// `Ambiguous` and `NeedsBuild` are typed resolution verdicts that replace the
/// free-text `Unavailable` messages the parent used to string-match: a name is
/// genuinely multiply-defined (`Ambiguous`, with the competing declarations) or
/// could not resolve because the open environment is incomplete (`NeedsBuild`,
/// naming the unbuilt imports). `Unavailable` remains for the residual
/// non-resolution failures (no proof position matched the selector, the
/// resolved declaration is not in the snapshot).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerProofStateResult {
    State {
        info: Box<LeanWorkerProofStateInfo>,
    },
    Unavailable {
        message: String,
    },
    Ambiguous {
        candidates: Vec<LeanWorkerDeclarationTargetInfo>,
    },
    NeedsBuild {
        missing: Vec<String>,
    },
}

/// Result for `LeanWorkerModuleQuerySelector::SurroundingDeclaration`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerSurroundingDeclarationResult {
    Declaration { info: LeanWorkerDeclarationTargetInfo },
    None,
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

/// Typed payload returned by one successful batch selector.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", content = "body", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerModuleQueryBatchResult {
    Diagnostics(LeanWorkerElabFailure),
    ProofState(LeanWorkerProofStateResult),
    TypeAt(LeanWorkerTypeAtResult),
    References(LeanWorkerReferencesResult),
    DeclarationTarget(LeanWorkerDeclarationTargetResult),
    SurroundingDeclaration(LeanWorkerSurroundingDeclarationResult),
}

/// One selector result in a batched module query.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerModuleQueryBatchItem {
    Ok {
        id: String,
        result: Box<LeanWorkerModuleQueryBatchResult>,
    },
    Unavailable {
        id: String,
        message: String,
    },
    BudgetExceeded {
        id: String,
        message: String,
    },
}

/// Successful batch selector envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerModuleQueryBatchEnvelope {
    pub items: Vec<LeanWorkerModuleQueryBatchItem>,
    pub total_truncated: bool,
}

/// Worker-side module snapshot cache status for a batched module query.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerModuleCacheStatus {
    Hit,
    Miss,
    Rebuilt,
    Evicted,
}

/// Phase timings for a batched module query, measured in the worker child.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerModuleQueryTimings {
    pub header_import_micros: u64,
    pub elaboration_micros: u64,
    pub projection_micros: u64,
    pub rendering_micros: u64,
}

impl LeanWorkerModuleQueryTimings {
    #[must_use]
    pub fn zero() -> Self {
        Self {
            header_import_micros: 0,
            elaboration_micros: 0,
            projection_micros: 0,
            rendering_micros: 0,
        }
    }
}

/// Cache and timing facts attached to a batched module query outcome.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerModuleQueryCacheFacts {
    pub cache_status: LeanWorkerModuleCacheStatus,
    pub timings: LeanWorkerModuleQueryTimings,
    pub output_bytes: u64,
    pub cache_entry_count: Option<u64>,
    pub cache_approx_bytes: Option<u64>,
}

impl LeanWorkerModuleQueryCacheFacts {
    #[must_use]
    pub fn uncached(output_bytes: u64) -> Self {
        Self {
            cache_status: LeanWorkerModuleCacheStatus::Miss,
            timings: LeanWorkerModuleQueryTimings::zero(),
            output_bytes,
            cache_entry_count: None,
            cache_approx_bytes: None,
        }
    }
}

/// Result of manually clearing the worker-side module snapshot cache.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeanWorkerModuleSnapshotCacheClearResult {
    pub entries_cleared: u64,
    pub approx_bytes_cleared: u64,
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

/// Outcome of `LeanWorkerSession::process_module_query_batch`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeanWorkerModuleQueryBatchOutcome {
    Ok {
        result: LeanWorkerModuleQueryBatchEnvelope,
        imports: Vec<String>,
        facts: LeanWorkerModuleQueryCacheFacts,
    },
    MissingImports {
        result: LeanWorkerModuleQueryBatchEnvelope,
        imports: Vec<String>,
        missing: Vec<String>,
        facts: LeanWorkerModuleQueryCacheFacts,
    },
    HeaderParseFailed {
        diagnostics: LeanWorkerElabFailure,
        facts: LeanWorkerModuleQueryCacheFacts,
    },
    /// The loaded capability dylib does not export
    /// `lean_rs_host_process_module_query_batch`.
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
