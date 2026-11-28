//! Bounded module-query projections returned by
//! [`crate::LeanSession::process_module_query`].
//!
//! Callers choose a query shape; Lean owns module-header handling,
//! elaboration, info-tree traversal, cursor selection, and bounded
//! rendering. Rust decodes only the requested projection.

use lean_rs::Obj;
use lean_rs::abi::nat;
use lean_rs::abi::structure::{take_ctor_objects, view};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
pub use lean_rs::host_bridge::module_requests::{
    DeclarationVerificationBatchItem, DeclarationVerificationBatchRequest, DeclarationVerificationRequest,
    DeclarationVerificationTarget, ModuleQuery, ModuleQueryOutputBudgets, ModuleQuerySelector, ModuleSourceSpan,
    ProofAttemptRequest, ProofCandidate, ProofEditTarget, ProofPositionSelector, SorryPolicy,
};

use crate::host::elaboration::LeanElabFailure;

/// Per-candidate proof attempt status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProofAttemptStatus {
    Closed,
    Progressed,
    Failed,
    Timeout,
    BudgetExceeded,
    NotAttempted,
    Unsupported,
}

/// Per-candidate proof attempt result row.
#[derive(Clone, Debug)]
pub struct ProofAttemptRow {
    pub id: String,
    pub status: ProofAttemptStatus,
    pub candidate_text: RenderedInfo,
    pub diagnostics: LeanElabFailure,
    pub downstream_diagnostics: LeanElabFailure,
    pub goals: Vec<RenderedInfo>,
    pub declaration: Option<DeclarationTargetInfo>,
    pub proof_position: Option<ProofPositionSummary>,
    pub output_truncated: bool,
}

/// Informational summary of the resolved proof position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofPositionSummary {
    pub index: u32,
    pub tactic: RenderedInfo,
}

/// Valid proof-state boundary a selector can target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofBoundaryCandidate {
    pub index: u32,
    pub kind: String,
    pub source: ModuleSourceSpan,
    pub excerpt: RenderedInfo,
}

/// Envelope for a bounded proof attempt.
#[derive(Clone, Debug)]
pub struct ProofAttemptEnvelope {
    pub candidates: Vec<ProofAttemptRow>,
    pub candidate_limit: u32,
    pub candidates_truncated: bool,
    /// Goal state at the resolved proof position before any candidate ran —
    /// the selected tactic's `goals_before`, identical to what the
    /// proof-position query reports as `goals_before` at the same position,
    /// rendered once per batch by the shim. Empty when the entry state is
    /// degraded or unresolvable (resolution failure or the source-text
    /// fallback).
    pub entry_goals: Vec<RenderedInfo>,
    /// Local hypotheses at the resolved proof position, rendered once per
    /// batch with the proof-position query's pretty locals mode from the same
    /// `goals_before` state — identical to what the proof-position query
    /// reports as `locals` at the same position. Empty under the same
    /// conditions as `entry_goals`.
    pub locals: Vec<LocalInfo>,
}

/// Header-aware proof attempt outcome.
#[derive(Clone, Debug)]
pub enum ProofAttemptOutcome {
    Ok {
        result: ProofAttemptEnvelope,
        imports: Vec<String>,
    },
    MissingImports {
        result: ProofAttemptEnvelope,
        imports: Vec<String>,
        missing: Vec<String>,
    },
    HeaderParseFailed {
        diagnostics: LeanElabFailure,
    },
    Unsupported,
}

/// Verification policy result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationVerificationStatus {
    Accepted,
    Rejected,
    NotFound,
    Ambiguous,
    Timeout,
    BudgetExceeded,
    Unsupported,
    NeedsBuild,
}

/// Bounded facts returned by declaration verification.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "verification booleans are independent wire facts for policy decisions"
)]
pub struct DeclarationVerificationFacts {
    pub target: Option<DeclarationTargetInfo>,
    pub diagnostics: LeanElabFailure,
    pub unresolved_goals: Vec<RenderedInfo>,
    pub contains_sorry: bool,
    pub contains_admit: bool,
    pub contains_sorry_ax: bool,
    pub axioms: Vec<String>,
    pub axioms_truncated: bool,
    pub output_truncated: bool,
    pub candidates: Vec<DeclarationTargetInfo>,
    pub axioms_available: bool,
}

/// Header-aware declaration verification outcome.
#[derive(Clone, Debug)]
pub enum DeclarationVerificationOutcome {
    Ok {
        status: DeclarationVerificationStatus,
        facts: Box<DeclarationVerificationFacts>,
        imports: Vec<String>,
    },
    MissingImports {
        status: DeclarationVerificationStatus,
        facts: Box<DeclarationVerificationFacts>,
        imports: Vec<String>,
        missing: Vec<String>,
    },
    HeaderParseFailed {
        diagnostics: LeanElabFailure,
    },
    Unsupported,
}

/// One ordered row inside a batch declaration-verification result.
#[derive(Clone, Debug)]
pub struct DeclarationVerificationBatchRow {
    pub id: String,
    pub target: DeclarationVerificationTarget,
    pub status: DeclarationVerificationStatus,
    pub facts: Box<DeclarationVerificationFacts>,
}

/// Header-aware batch declaration-verification outcome.
#[derive(Clone, Debug)]
pub enum DeclarationVerificationBatchOutcome {
    Ok {
        results: Vec<DeclarationVerificationBatchRow>,
        imports: Vec<String>,
    },
    MissingImports {
        results: Vec<DeclarationVerificationBatchRow>,
        imports: Vec<String>,
        missing: Vec<String>,
    },
    HeaderParseFailed {
        diagnostics: LeanElabFailure,
    },
    Unsupported,
}

/// Bounded rendered Lean text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedInfo {
    pub value: String,
    pub truncated: bool,
}

impl<'lean> TryFromLean<'lean> for RenderedInfo {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let truncated = bool_tail(&obj, 0, "RenderedInfo.truncated")?;
        let [value] = take_ctor_objects::<1>(obj, 0, "RenderedInfo")?;
        Ok(Self {
            value: String::try_from_lean(value)?,
            truncated,
        })
    }
}

/// One identifier occurrence the elaborator recorded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameRefNode {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub name: String,
    pub is_binder: bool,
}

impl<'lean> TryFromLean<'lean> for NameRefNode {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let is_binder = bool_tail(&obj, 0, "NameRefNode.isBinder")?;
        let [sl, sc, el, ec, nm] = take_ctor_objects::<5>(obj, 0, "NameRefNode")?;
        Ok(Self {
            start_line: u32::try_from_lean(sl)?,
            start_column: u32::try_from_lean(sc)?,
            end_line: u32::try_from_lean(el)?,
            end_column: u32::try_from_lean(ec)?,
            name: String::try_from_lean(nm)?,
            is_binder,
        })
    }
}

/// Result for [`ModuleQuery::TypeAt`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeAtResult {
    Term {
        span: ModuleSourceSpan,
        expr: RenderedInfo,
        type_str: RenderedInfo,
        expected_type: Option<RenderedInfo>,
    },
    NoTerm,
}

impl<'lean> TryFromLean<'lean> for TypeAtResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [span, expr, type_str, expected_type] = take_ctor_objects::<4>(obj, 0, "TypeAtResult::term")?;
                Ok(Self::Term {
                    span: ModuleSourceSpan::try_from_lean(span)?,
                    expr: RenderedInfo::try_from_lean(expr)?,
                    type_str: RenderedInfo::try_from_lean(type_str)?,
                    expected_type: Option::<RenderedInfo>::try_from_lean(expected_type)?,
                })
            }
            1 => Ok(Self::NoTerm),
            other => Err(conversion_error(format!(
                "expected Lean TypeAtResult ctor (tag 0..=1), found tag {other}"
            ))),
        }
    }
}

/// Result for [`ModuleQuery::GoalAt`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GoalAtResult {
    Goal {
        span: ModuleSourceSpan,
        goals_before: Vec<String>,
        goals_after: Vec<String>,
        truncated: bool,
    },
    NoTacticContext,
}

impl<'lean> TryFromLean<'lean> for GoalAtResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let truncated = bool_tail(&obj, 0, "GoalAtResult::goal.truncated")?;
                let [span, before, after] = take_ctor_objects::<3>(obj, 0, "GoalAtResult::goal")?;
                Ok(Self::Goal {
                    span: ModuleSourceSpan::try_from_lean(span)?,
                    goals_before: Vec::<String>::try_from_lean(before)?,
                    goals_after: Vec::<String>::try_from_lean(after)?,
                    truncated,
                })
            }
            1 => Ok(Self::NoTacticContext),
            other => Err(conversion_error(format!(
                "expected Lean GoalAtResult ctor (tag 0..=1), found tag {other}"
            ))),
        }
    }
}

/// Result for [`ModuleQuery::References`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferencesResult {
    pub references: Vec<NameRefNode>,
    pub truncated: bool,
}

impl<'lean> TryFromLean<'lean> for ReferencesResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let truncated = bool_tail(&obj, 0, "ReferencesResult.truncated")?;
        let [references] = take_ctor_objects::<1>(obj, 0, "ReferencesResult")?;
        Ok(Self {
            references: Vec::<NameRefNode>::try_from_lean(references)?,
            truncated,
        })
    }
}

/// One rendered local declaration in a proof-state result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalInfo {
    pub name: String,
    pub binder_info: String,
    pub type_str: RenderedInfo,
    pub value: Option<RenderedInfo>,
}

impl<'lean> TryFromLean<'lean> for LocalInfo {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [name, binder_info, type_str, value] = take_ctor_objects::<4>(obj, 0, "LocalInfo")?;
        Ok(Self {
            name: String::try_from_lean(name)?,
            binder_info: String::try_from_lean(binder_info)?,
            type_str: RenderedInfo::try_from_lean(type_str)?,
            value: Option::<RenderedInfo>::try_from_lean(value)?,
        })
    }
}

/// Source metadata for the declaration a query targets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationTargetInfo {
    pub short_name: String,
    pub declaration_name: String,
    pub namespace_name: String,
    pub declaration_kind: String,
    pub declaration_span: ModuleSourceSpan,
    pub name_span: ModuleSourceSpan,
    pub body_span: ModuleSourceSpan,
}

impl<'lean> TryFromLean<'lean> for DeclarationTargetInfo {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [
            short_name,
            declaration_name,
            namespace_name,
            declaration_kind,
            declaration_span,
            name_span,
            body_span,
        ] = take_ctor_objects::<7>(obj, 0, "DeclarationTargetInfo")?;
        Ok(Self {
            short_name: String::try_from_lean(short_name)?,
            declaration_name: String::try_from_lean(declaration_name)?,
            namespace_name: String::try_from_lean(namespace_name)?,
            declaration_kind: String::try_from_lean(declaration_kind)?,
            declaration_span: ModuleSourceSpan::try_from_lean(declaration_span)?,
            name_span: ModuleSourceSpan::try_from_lean(name_span)?,
            body_span: ModuleSourceSpan::try_from_lean(body_span)?,
        })
    }
}

/// Result for [`ModuleQuerySelector::DeclarationTarget`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclarationTargetResult {
    Target(DeclarationTargetInfo),
    NotFound,
    Ambiguous(Vec<DeclarationTargetInfo>),
}

impl<'lean> TryFromLean<'lean> for DeclarationTargetResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [info] = take_ctor_objects::<1>(obj, 0, "DeclarationTargetResult::target")?;
                Ok(Self::Target(DeclarationTargetInfo::try_from_lean(info)?))
            }
            1 => Ok(Self::NotFound),
            2 => {
                let [candidates] = take_ctor_objects::<1>(obj, 2, "DeclarationTargetResult::ambiguous")?;
                Ok(Self::Ambiguous(Vec::<DeclarationTargetInfo>::try_from_lean(
                    candidates,
                )?))
            }
            other => Err(conversion_error(format!(
                "expected Lean DeclarationTargetResult ctor (tag 0..=2), found tag {other}"
            ))),
        }
    }
}

/// Result for [`ModuleQuerySelector::DeclarationOutline`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationOutlineResult {
    pub declarations: Vec<DeclarationTargetInfo>,
    pub truncated: bool,
}

impl<'lean> TryFromLean<'lean> for DeclarationOutlineResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let truncated = bool_tail(&obj, 0, "DeclarationOutlineResult.truncated")?;
        let [declarations] = take_ctor_objects::<1>(obj, 0, "DeclarationOutlineResult")?;
        Ok(Self {
            declarations: Vec::<DeclarationTargetInfo>::try_from_lean(declarations)?,
            truncated,
        })
    }
}

impl<'lean> TryFromLean<'lean> for ProofAttemptStatus {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        Self::from_scalar(sum_tag(&obj)?)
    }
}

impl ProofAttemptStatus {
    fn from_scalar(value: u8) -> lean_rs::LeanResult<Self> {
        match value {
            0 => Ok(Self::Closed),
            1 => Ok(Self::Progressed),
            2 => Ok(Self::Failed),
            3 => Ok(Self::Timeout),
            4 => Ok(Self::BudgetExceeded),
            5 => Ok(Self::NotAttempted),
            6 => Ok(Self::Unsupported),
            other => Err(conversion_error(format!(
                "expected Lean ProofAttemptStatus ctor (tag 0..=6), found tag {other}"
            ))),
        }
    }
}

impl<'lean> TryFromLean<'lean> for ProofPositionSummary {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [index, tactic] = take_ctor_objects::<2>(obj, 0, "ProofPositionSummary")?;
        Ok(Self {
            index: u32::try_from_lean(index)?,
            tactic: RenderedInfo::try_from_lean(tactic)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for ProofBoundaryCandidate {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [index, kind, source, excerpt] = take_ctor_objects::<4>(obj, 0, "ProofBoundaryCandidate")?;
        Ok(Self {
            index: u32::try_from_lean(index)?,
            kind: String::try_from_lean(kind)?,
            source: ModuleSourceSpan::try_from_lean(source)?,
            excerpt: RenderedInfo::try_from_lean(excerpt)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for ProofAttemptRow {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let ctor = view(&obj).ctor_shape(0, 7, "ProofAttemptRow")?;
        let status = ProofAttemptStatus::from_scalar(ctor.uint8(0, "ProofAttemptRow.status")?)?;
        let output_truncated = ctor.bool(1, "ProofAttemptRow.outputTruncated")?;
        let [
            id,
            candidate_text,
            diagnostics,
            downstream_diagnostics,
            goals,
            declaration,
            proof_position,
        ] = take_ctor_objects::<7>(obj, 0, "ProofAttemptRow")?;
        Ok(Self {
            id: String::try_from_lean(id)?,
            status,
            candidate_text: RenderedInfo::try_from_lean(candidate_text)?,
            diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
            downstream_diagnostics: LeanElabFailure::try_from_lean(downstream_diagnostics)?,
            goals: Vec::<RenderedInfo>::try_from_lean(goals)?,
            declaration: Option::<DeclarationTargetInfo>::try_from_lean(declaration)?,
            proof_position: Option::<ProofPositionSummary>::try_from_lean(proof_position)?,
            output_truncated,
        })
    }
}

impl<'lean> TryFromLean<'lean> for ProofAttemptEnvelope {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let candidates_truncated = bool_tail(&obj, 0, "ProofAttemptEnvelope.candidatesTruncated")?;
        let [candidates, candidate_limit, entry_goals, locals] =
            take_ctor_objects::<4>(obj, 0, "ProofAttemptEnvelope")?;
        Ok(Self {
            candidates: Vec::<ProofAttemptRow>::try_from_lean(candidates)?,
            candidate_limit: u32::try_from_lean(candidate_limit)?,
            candidates_truncated,
            entry_goals: Vec::<RenderedInfo>::try_from_lean(entry_goals)?,
            locals: Vec::<LocalInfo>::try_from_lean(locals)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for ProofAttemptOutcome {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [result, imports] = take_ctor_objects::<2>(obj, 0, "ProofAttemptOutcome::ok")?;
                Ok(Self::Ok {
                    result: ProofAttemptEnvelope::try_from_lean(result)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                })
            }
            1 => {
                let [result, imports, missing] = take_ctor_objects::<3>(obj, 1, "ProofAttemptOutcome::missingImports")?;
                Ok(Self::MissingImports {
                    result: ProofAttemptEnvelope::try_from_lean(result)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                    missing: Vec::<String>::try_from_lean(missing)?,
                })
            }
            2 => {
                let [diagnostics] = take_ctor_objects::<1>(obj, 2, "ProofAttemptOutcome::headerParseFailed")?;
                Ok(Self::HeaderParseFailed {
                    diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
                })
            }
            3 => Ok(Self::Unsupported),
            other => Err(conversion_error(format!(
                "expected Lean ProofAttemptOutcome ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationVerificationStatus {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        Self::from_scalar(sum_tag(&obj)?)
    }
}

impl DeclarationVerificationStatus {
    fn from_scalar(value: u8) -> lean_rs::LeanResult<Self> {
        match value {
            0 => Ok(Self::Accepted),
            1 => Ok(Self::Rejected),
            2 => Ok(Self::NotFound),
            3 => Ok(Self::Ambiguous),
            4 => Ok(Self::Timeout),
            5 => Ok(Self::BudgetExceeded),
            6 => Ok(Self::Unsupported),
            7 => Ok(Self::NeedsBuild),
            other => Err(conversion_error(format!(
                "expected Lean DeclarationVerificationStatus ctor (tag 0..=7), found tag {other}"
            ))),
        }
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationVerificationFacts {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let contains_sorry = bool_tail(&obj, 0, "DeclarationVerificationFacts.containsSorry")?;
        let contains_admit = bool_tail(&obj, 1, "DeclarationVerificationFacts.containsAdmit")?;
        let contains_sorry_ax = bool_tail(&obj, 2, "DeclarationVerificationFacts.containsSorryAx")?;
        let axioms_truncated = bool_tail(&obj, 3, "DeclarationVerificationFacts.axiomsTruncated")?;
        let output_truncated = bool_tail(&obj, 4, "DeclarationVerificationFacts.outputTruncated")?;
        let axioms_available = bool_tail(&obj, 5, "DeclarationVerificationFacts.axiomsAvailable")?;
        let [target, diagnostics, unresolved_goals, axioms, candidates] =
            take_ctor_objects::<5>(obj, 0, "DeclarationVerificationFacts")?;
        Ok(Self {
            target: Option::<DeclarationTargetInfo>::try_from_lean(target)?,
            diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
            unresolved_goals: Vec::<RenderedInfo>::try_from_lean(unresolved_goals)?,
            contains_sorry,
            contains_admit,
            contains_sorry_ax,
            axioms: Vec::<String>::try_from_lean(axioms)?,
            axioms_truncated,
            output_truncated,
            candidates: Vec::<DeclarationTargetInfo>::try_from_lean(candidates)?,
            axioms_available,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationVerificationOutcome {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let ctor = view(&obj).ctor_shape(0, 2, "DeclarationVerificationOutcome::ok")?;
                let status = DeclarationVerificationStatus::from_scalar(
                    ctor.uint8(0, "DeclarationVerificationOutcome::ok.status")?,
                )?;
                let [facts, imports] = take_ctor_objects::<2>(obj, 0, "DeclarationVerificationOutcome::ok")?;
                Ok(Self::Ok {
                    status,
                    facts: Box::new(DeclarationVerificationFacts::try_from_lean(facts)?),
                    imports: Vec::<String>::try_from_lean(imports)?,
                })
            }
            1 => {
                let ctor = view(&obj).ctor_shape(1, 3, "DeclarationVerificationOutcome::missingImports")?;
                let status = DeclarationVerificationStatus::from_scalar(
                    ctor.uint8(0, "DeclarationVerificationOutcome::missingImports.status")?,
                )?;
                let [facts, imports, missing] =
                    take_ctor_objects::<3>(obj, 1, "DeclarationVerificationOutcome::missingImports")?;
                Ok(Self::MissingImports {
                    status,
                    facts: Box::new(DeclarationVerificationFacts::try_from_lean(facts)?),
                    imports: Vec::<String>::try_from_lean(imports)?,
                    missing: Vec::<String>::try_from_lean(missing)?,
                })
            }
            2 => {
                let [diagnostics] =
                    take_ctor_objects::<1>(obj, 2, "DeclarationVerificationOutcome::headerParseFailed")?;
                Ok(Self::HeaderParseFailed {
                    diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
                })
            }
            3 => Ok(Self::Unsupported),
            other => Err(conversion_error(format!(
                "expected Lean DeclarationVerificationOutcome ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationVerificationBatchRow {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let ctor = view(&obj).ctor_shape(0, 3, "DeclarationVerificationBatchRow")?;
        let status =
            DeclarationVerificationStatus::from_scalar(ctor.uint8(0, "DeclarationVerificationBatchRow.status")?)?;
        let [id, target, facts] = take_ctor_objects::<3>(obj, 0, "DeclarationVerificationBatchRow")?;
        Ok(Self {
            id: String::try_from_lean(id)?,
            target: DeclarationVerificationTarget::try_from_lean(target)?,
            status,
            facts: Box::new(DeclarationVerificationFacts::try_from_lean(facts)?),
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationVerificationBatchOutcome {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [results, imports] = take_ctor_objects::<2>(obj, 0, "DeclarationVerificationBatchOutcome::ok")?;
                Ok(Self::Ok {
                    results: Vec::<DeclarationVerificationBatchRow>::try_from_lean(results)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                })
            }
            1 => {
                let [results, imports, missing] =
                    take_ctor_objects::<3>(obj, 1, "DeclarationVerificationBatchOutcome::missingImports")?;
                Ok(Self::MissingImports {
                    results: Vec::<DeclarationVerificationBatchRow>::try_from_lean(results)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                    missing: Vec::<String>::try_from_lean(missing)?,
                })
            }
            2 => {
                let [diagnostics] =
                    take_ctor_objects::<1>(obj, 2, "DeclarationVerificationBatchOutcome::headerParseFailed")?;
                Ok(Self::HeaderParseFailed {
                    diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
                })
            }
            3 => Ok(Self::Unsupported),
            other => Err(conversion_error(format!(
                "expected Lean DeclarationVerificationBatchOutcome ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

/// Proof-state payload for one cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofStateInfo {
    pub declaration_name: Option<String>,
    pub namespace_name: String,
    pub safe_edit: Option<DeclarationTargetInfo>,
    pub span: ModuleSourceSpan,
    pub goals_before: Vec<String>,
    pub goals_after: Vec<String>,
    pub locals: Vec<LocalInfo>,
    pub expected_type: Option<RenderedInfo>,
    pub truncated: bool,
    pub proof_boundaries: Vec<ProofBoundaryCandidate>,
    pub proof_boundaries_truncated: bool,
}

impl<'lean> TryFromLean<'lean> for ProofStateInfo {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let truncated = bool_tail(&obj, 0, "ProofStateInfo.truncated")?;
        let proof_boundaries_truncated = bool_tail(&obj, 1, "ProofStateInfo.proofBoundariesTruncated")?;
        let [
            declaration_name,
            namespace_name,
            safe_edit,
            span,
            goals_before,
            goals_after,
            locals,
            expected_type,
            proof_boundaries,
        ] = take_ctor_objects::<9>(obj, 0, "ProofStateInfo")?;
        Ok(Self {
            declaration_name: Option::<String>::try_from_lean(declaration_name)?,
            namespace_name: String::try_from_lean(namespace_name)?,
            safe_edit: Option::<DeclarationTargetInfo>::try_from_lean(safe_edit)?,
            span: ModuleSourceSpan::try_from_lean(span)?,
            goals_before: Vec::<String>::try_from_lean(goals_before)?,
            goals_after: Vec::<String>::try_from_lean(goals_after)?,
            locals: Vec::<LocalInfo>::try_from_lean(locals)?,
            expected_type: Option::<RenderedInfo>::try_from_lean(expected_type)?,
            truncated,
            proof_boundaries: Vec::<ProofBoundaryCandidate>::try_from_lean(proof_boundaries)?,
            proof_boundaries_truncated,
        })
    }
}

/// Result for [`ModuleQuerySelector::ProofState`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProofStateResult {
    State(Box<ProofStateInfo>),
    Unavailable {
        message: String,
        proof_boundaries: Vec<ProofBoundaryCandidate>,
        proof_boundaries_truncated: bool,
    },
    Ambiguous {
        candidates: Vec<DeclarationTargetInfo>,
    },
    NeedsBuild {
        missing: Vec<String>,
    },
}

impl<'lean> TryFromLean<'lean> for ProofStateResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [info] = take_ctor_objects::<1>(obj, 0, "ProofStateResult::state")?;
                Ok(Self::State(Box::new(ProofStateInfo::try_from_lean(info)?)))
            }
            1 => {
                let proof_boundaries_truncated = view(&obj)
                    .ctor_shape(1, 2, "ProofStateResult::unavailable")?
                    .bool(0, "ProofStateResult::unavailable.proofBoundariesTruncated")?;
                let [message, proof_boundaries] = take_ctor_objects::<2>(obj, 1, "ProofStateResult::unavailable")?;
                Ok(Self::Unavailable {
                    message: String::try_from_lean(message)?,
                    proof_boundaries: Vec::<ProofBoundaryCandidate>::try_from_lean(proof_boundaries)?,
                    proof_boundaries_truncated,
                })
            }
            2 => {
                let [candidates] = take_ctor_objects::<1>(obj, 2, "ProofStateResult::ambiguous")?;
                Ok(Self::Ambiguous {
                    candidates: Vec::<DeclarationTargetInfo>::try_from_lean(candidates)?,
                })
            }
            3 => {
                let [missing] = take_ctor_objects::<1>(obj, 3, "ProofStateResult::needsBuild")?;
                Ok(Self::NeedsBuild {
                    missing: Vec::<String>::try_from_lean(missing)?,
                })
            }
            other => Err(conversion_error(format!(
                "expected Lean ProofStateResult ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

/// Result for [`ModuleQuerySelector::SurroundingDeclaration`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SurroundingDeclarationResult {
    Declaration(DeclarationTargetInfo),
    None,
}

impl<'lean> TryFromLean<'lean> for SurroundingDeclarationResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [info] = take_ctor_objects::<1>(obj, 0, "SurroundingDeclarationResult::declaration")?;
                Ok(Self::Declaration(DeclarationTargetInfo::try_from_lean(info)?))
            }
            1 => Ok(Self::None),
            other => Err(conversion_error(format!(
                "expected Lean SurroundingDeclarationResult ctor (tag 0..=1), found tag {other}"
            ))),
        }
    }
}

/// Typed payload returned by a successful module query.
#[derive(Clone, Debug)]
pub enum ModuleQueryResult {
    Diagnostics(LeanElabFailure),
    TypeAt(TypeAtResult),
    GoalAt(GoalAtResult),
    References(ReferencesResult),
}

impl<'lean> TryFromLean<'lean> for ModuleQueryResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [failure] = take_ctor_objects::<1>(obj, 0, "ModuleQueryResult::diagnostics")?;
                Ok(Self::Diagnostics(LeanElabFailure::try_from_lean(failure)?))
            }
            1 => {
                let [result] = take_ctor_objects::<1>(obj, 1, "ModuleQueryResult::typeAt")?;
                Ok(Self::TypeAt(TypeAtResult::try_from_lean(result)?))
            }
            2 => {
                let [result] = take_ctor_objects::<1>(obj, 2, "ModuleQueryResult::goalAt")?;
                Ok(Self::GoalAt(GoalAtResult::try_from_lean(result)?))
            }
            3 => {
                let [result] = take_ctor_objects::<1>(obj, 3, "ModuleQueryResult::references")?;
                Ok(Self::References(ReferencesResult::try_from_lean(result)?))
            }
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryResult ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

/// Typed payload returned by one successful batch selector.
#[derive(Clone, Debug)]
pub enum ModuleQueryBatchResult {
    Diagnostics(LeanElabFailure),
    ProofState(ProofStateResult),
    TypeAt(TypeAtResult),
    References(ReferencesResult),
    DeclarationTarget(DeclarationTargetResult),
    SurroundingDeclaration(SurroundingDeclarationResult),
    DeclarationOutline(DeclarationOutlineResult),
}

impl<'lean> TryFromLean<'lean> for ModuleQueryBatchResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [failure] = take_ctor_objects::<1>(obj, 0, "ModuleQueryBatchResult::diagnostics")?;
                Ok(Self::Diagnostics(LeanElabFailure::try_from_lean(failure)?))
            }
            1 => {
                let [result] = take_ctor_objects::<1>(obj, 1, "ModuleQueryBatchResult::proofState")?;
                Ok(Self::ProofState(ProofStateResult::try_from_lean(result)?))
            }
            2 => {
                let [result] = take_ctor_objects::<1>(obj, 2, "ModuleQueryBatchResult::typeAt")?;
                Ok(Self::TypeAt(TypeAtResult::try_from_lean(result)?))
            }
            3 => {
                let [result] = take_ctor_objects::<1>(obj, 3, "ModuleQueryBatchResult::references")?;
                Ok(Self::References(ReferencesResult::try_from_lean(result)?))
            }
            4 => {
                let [result] = take_ctor_objects::<1>(obj, 4, "ModuleQueryBatchResult::declarationTarget")?;
                Ok(Self::DeclarationTarget(DeclarationTargetResult::try_from_lean(result)?))
            }
            5 => {
                let [result] = take_ctor_objects::<1>(obj, 5, "ModuleQueryBatchResult::surroundingDeclaration")?;
                Ok(Self::SurroundingDeclaration(
                    SurroundingDeclarationResult::try_from_lean(result)?,
                ))
            }
            6 => {
                let [result] = take_ctor_objects::<1>(obj, 6, "ModuleQueryBatchResult::declarationOutline")?;
                Ok(Self::DeclarationOutline(DeclarationOutlineResult::try_from_lean(
                    result,
                )?))
            }
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryBatchResult ctor (tag 0..=6), found tag {other}"
            ))),
        }
    }
}

/// One selector result in a batched module query.
#[derive(Clone, Debug)]
pub enum ModuleQueryBatchItem {
    Ok {
        id: String,
        result: Box<ModuleQueryBatchResult>,
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

impl ModuleQueryBatchItem {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Ok { id, .. } | Self::Unavailable { id, .. } | Self::BudgetExceeded { id, .. } => id,
        }
    }
}

impl<'lean> TryFromLean<'lean> for ModuleQueryBatchItem {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [id, result] = take_ctor_objects::<2>(obj, 0, "ModuleQueryBatchItem::ok")?;
                Ok(Self::Ok {
                    id: String::try_from_lean(id)?,
                    result: Box::new(ModuleQueryBatchResult::try_from_lean(result)?),
                })
            }
            1 => {
                let [id, message] = take_ctor_objects::<2>(obj, 1, "ModuleQueryBatchItem::unavailable")?;
                Ok(Self::Unavailable {
                    id: String::try_from_lean(id)?,
                    message: String::try_from_lean(message)?,
                })
            }
            2 => {
                let [id, message] = take_ctor_objects::<2>(obj, 2, "ModuleQueryBatchItem::budgetExceeded")?;
                Ok(Self::BudgetExceeded {
                    id: String::try_from_lean(id)?,
                    message: String::try_from_lean(message)?,
                })
            }
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryBatchItem ctor (tag 0..=2), found tag {other}"
            ))),
        }
    }
}

/// Successful batch selector envelope.
#[derive(Clone, Debug)]
pub struct ModuleQueryBatchEnvelope {
    pub items: Vec<ModuleQueryBatchItem>,
    pub total_truncated: bool,
}

impl<'lean> TryFromLean<'lean> for ModuleQueryBatchEnvelope {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let total_truncated = bool_tail(&obj, 0, "ModuleQueryBatchEnvelope.totalTruncated")?;
        let [items] = take_ctor_objects::<1>(obj, 0, "ModuleQueryBatchEnvelope")?;
        Ok(Self {
            items: Vec::<ModuleQueryBatchItem>::try_from_lean(items)?,
            total_truncated,
        })
    }
}

/// Worker-side module snapshot cache status for a batched module query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleQueryCacheStatus {
    Hit,
    Miss,
    Rebuilt,
    Evicted,
}

impl<'lean> TryFromLean<'lean> for ModuleQueryCacheStatus {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => Ok(Self::Hit),
            1 => Ok(Self::Miss),
            2 => Ok(Self::Rebuilt),
            3 => Ok(Self::Evicted),
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryCacheStatus ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

impl ModuleQueryCacheStatus {
    fn from_scalar_tail(byte: u8) -> lean_rs::LeanResult<Self> {
        match byte {
            0 => Ok(Self::Hit),
            1 => Ok(Self::Miss),
            2 => Ok(Self::Rebuilt),
            3 => Ok(Self::Evicted),
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryCacheStatus scalar tag 0..=3, found {other}"
            ))),
        }
    }
}

/// Phase timings for cached batched module queries, in microseconds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleQueryTimings {
    pub header_import_micros: u64,
    pub elaboration_micros: u64,
    pub projection_micros: u64,
    pub rendering_micros: u64,
}

impl<'lean> TryFromLean<'lean> for ModuleQueryTimings {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let ctor = view(&obj).ctor_shape(0, 0, "ModuleQueryTimings")?;
        Ok(Self {
            header_import_micros: ctor.uint64(0, "ModuleQueryTimings.headerImportMicros")?,
            elaboration_micros: ctor.uint64(8, "ModuleQueryTimings.elaborationMicros")?,
            projection_micros: ctor.uint64(16, "ModuleQueryTimings.projectionMicros")?,
            rendering_micros: ctor.uint64(24, "ModuleQueryTimings.renderingMicros")?,
        })
    }
}

/// Cache and timing facts attached to cached batched module-query outcomes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleQueryCacheFacts {
    pub cache_status: ModuleQueryCacheStatus,
    pub timings: ModuleQueryTimings,
    pub output_bytes: u64,
    pub cache_entry_count: Option<u64>,
    pub cache_approx_bytes: Option<u64>,
}

impl<'lean> TryFromLean<'lean> for ModuleQueryCacheFacts {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let ctor = view(&obj).ctor_shape(0, 3, "ModuleQueryCacheFacts")?;
        let output_bytes = ctor.uint64(0, "ModuleQueryCacheFacts.outputBytes")?;
        let cache_status = ctor.uint8(8, "ModuleQueryCacheFacts.cacheStatus")?;
        let [timings, cache_entry_count, cache_approx_bytes] = take_ctor_objects::<3>(obj, 0, "ModuleQueryCacheFacts")?;
        Ok(Self {
            cache_status: ModuleQueryCacheStatus::from_scalar_tail(cache_status)?,
            timings: ModuleQueryTimings::try_from_lean(timings)?,
            output_bytes,
            cache_entry_count: option_nat_u64(cache_entry_count)?,
            cache_approx_bytes: option_nat_u64(cache_approx_bytes)?,
        })
    }
}

/// Cache policy passed to the Lean-side module snapshot cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleQueryCachePolicy {
    pub file_identity: String,
    pub key: String,
    pub max_entries: u64,
    pub ttl_millis: u64,
    pub max_bytes: u64,
}

/// Header-aware module-query outcome.
#[derive(Clone, Debug)]
pub enum ModuleQueryOutcome {
    Ok {
        result: ModuleQueryResult,
        imports: Vec<String>,
    },
    MissingImports {
        result: ModuleQueryResult,
        imports: Vec<String>,
        missing: Vec<String>,
    },
    HeaderParseFailed {
        diagnostics: LeanElabFailure,
    },
    Unsupported,
}

impl<'lean> TryFromLean<'lean> for ModuleQueryOutcome {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [result, imports] = take_ctor_objects::<2>(obj, 0, "ModuleQueryOutcome::ok")?;
                Ok(Self::Ok {
                    result: ModuleQueryResult::try_from_lean(result)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                })
            }
            1 => {
                let [result, imports, missing] = take_ctor_objects::<3>(obj, 1, "ModuleQueryOutcome::missingImports")?;
                Ok(Self::MissingImports {
                    result: ModuleQueryResult::try_from_lean(result)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                    missing: Vec::<String>::try_from_lean(missing)?,
                })
            }
            2 => {
                let [diagnostics] = take_ctor_objects::<1>(obj, 2, "ModuleQueryOutcome::headerParseFailed")?;
                Ok(Self::HeaderParseFailed {
                    diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
                })
            }
            3 => Ok(Self::Unsupported),
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryOutcome ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

/// Header-aware batched module-query outcome.
#[derive(Clone, Debug)]
pub enum ModuleQueryBatchOutcome {
    Ok {
        result: ModuleQueryBatchEnvelope,
        imports: Vec<String>,
    },
    MissingImports {
        result: ModuleQueryBatchEnvelope,
        imports: Vec<String>,
        missing: Vec<String>,
    },
    HeaderParseFailed {
        diagnostics: LeanElabFailure,
    },
    Unsupported,
}

impl<'lean> TryFromLean<'lean> for ModuleQueryBatchOutcome {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [result, imports] = take_ctor_objects::<2>(obj, 0, "ModuleQueryBatchOutcome::ok")?;
                Ok(Self::Ok {
                    result: ModuleQueryBatchEnvelope::try_from_lean(result)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                })
            }
            1 => {
                let [result, imports, missing] =
                    take_ctor_objects::<3>(obj, 1, "ModuleQueryBatchOutcome::missingImports")?;
                Ok(Self::MissingImports {
                    result: ModuleQueryBatchEnvelope::try_from_lean(result)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                    missing: Vec::<String>::try_from_lean(missing)?,
                })
            }
            2 => {
                let [diagnostics] = take_ctor_objects::<1>(obj, 2, "ModuleQueryBatchOutcome::headerParseFailed")?;
                Ok(Self::HeaderParseFailed {
                    diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
                })
            }
            3 => Ok(Self::Unsupported),
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryBatchOutcome ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

/// Header-aware batched module-query outcome with cache/timing facts.
#[derive(Clone, Debug)]
pub enum ModuleQueryBatchCachedOutcome {
    Ok {
        result: ModuleQueryBatchEnvelope,
        imports: Vec<String>,
        facts: ModuleQueryCacheFacts,
    },
    MissingImports {
        result: ModuleQueryBatchEnvelope,
        imports: Vec<String>,
        missing: Vec<String>,
        facts: ModuleQueryCacheFacts,
    },
    HeaderParseFailed {
        diagnostics: LeanElabFailure,
        facts: ModuleQueryCacheFacts,
    },
    Unsupported,
}

impl<'lean> TryFromLean<'lean> for ModuleQueryBatchCachedOutcome {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [result, imports, facts] = take_ctor_objects::<3>(obj, 0, "ModuleQueryBatchCachedOutcome::ok")?;
                Ok(Self::Ok {
                    result: ModuleQueryBatchEnvelope::try_from_lean(result)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                    facts: ModuleQueryCacheFacts::try_from_lean(facts)?,
                })
            }
            1 => {
                let [result, imports, missing, facts] =
                    take_ctor_objects::<4>(obj, 1, "ModuleQueryBatchCachedOutcome::missingImports")?;
                Ok(Self::MissingImports {
                    result: ModuleQueryBatchEnvelope::try_from_lean(result)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                    missing: Vec::<String>::try_from_lean(missing)?,
                    facts: ModuleQueryCacheFacts::try_from_lean(facts)?,
                })
            }
            2 => {
                let [diagnostics, facts] =
                    take_ctor_objects::<2>(obj, 2, "ModuleQueryBatchCachedOutcome::headerParseFailed")?;
                Ok(Self::HeaderParseFailed {
                    diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
                    facts: ModuleQueryCacheFacts::try_from_lean(facts)?,
                })
            }
            3 => Ok(Self::Unsupported),
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryBatchCachedOutcome ctor (tag 0..=3), found tag {other}"
            ))),
        }
    }
}

/// Result of clearing the Lean-side module snapshot cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleSnapshotCacheClearResult {
    pub entries_cleared: u64,
    pub approx_bytes_cleared: u64,
}

impl<'lean> TryFromLean<'lean> for ModuleSnapshotCacheClearResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let ctor = view(&obj).ctor_shape(0, 0, "ModuleSnapshotCacheClearResult")?;
        Ok(Self {
            entries_cleared: ctor.uint64(0, "ModuleSnapshotCacheClearResult.entriesCleared")?,
            approx_bytes_cleared: ctor.uint64(8, "ModuleSnapshotCacheClearResult.approxBytesCleared")?,
        })
    }
}

fn option_nat_u64(obj: Obj<'_>) -> lean_rs::LeanResult<Option<u64>> {
    match sum_tag(&obj)? {
        0 => Ok(None),
        1 => {
            let [value] = take_ctor_objects::<1>(obj, 1, "Option::some Nat")?;
            Ok(Some(nat::try_to_u64(value)?))
        }
        other => Err(conversion_error(format!(
            "expected Lean Option Nat ctor (tag 0..=1), found tag {other}"
        ))),
    }
}

fn bool_tail(obj: &Obj<'_>, offset: u32, label: &str) -> lean_rs::LeanResult<bool> {
    let ctor = view(obj).ctor()?;
    if ctor.tag() != 0 {
        return Err(conversion_error(format!(
            "expected Lean {label} constructor tag 0, found tag {}",
            ctor.tag()
        )));
    }
    ctor.bool(offset, label)
}

fn sum_tag(obj: &Obj<'_>) -> lean_rs::LeanResult<u8> {
    view(obj).sum_tag()
}
