//! Bounded module-query projections returned by
//! [`crate::LeanSession::process_module_query`].
//!
//! Callers choose a query shape; Lean owns module-header handling,
//! elaboration, info-tree traversal, cursor selection, and bounded
//! rendering. Rust decodes only the requested projection.

use lean_rs::abi::nat;
use lean_rs::abi::structure::{alloc_ctor_with_objects, take_ctor_objects, view};
use lean_rs::abi::traits::{IntoLean, LeanAbi, TryFromLean, conversion_error, sealed};
use lean_rs::{LeanRuntime, Obj};

use crate::host::elaboration::LeanElabFailure;

/// Query shape for one header-aware Lean module processing request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleQuery {
    /// Return only diagnostics from elaborating the module.
    Diagnostics,
    /// Return type information for the innermost term covering `line:column`.
    TypeAt {
        /// 1-indexed line in the original source.
        line: u32,
        /// 1-indexed column in the original source.
        column: u32,
    },
    /// Return tactic goals for the innermost tactic context covering `line:column`.
    GoalAt {
        /// 1-indexed line in the original source.
        line: u32,
        /// 1-indexed column in the original source.
        column: u32,
    },
    /// Return binder/use-site references whose recorded name exactly matches `name`.
    References {
        /// Fully-qualified Lean name or binder name as the elaborator records it.
        name: String,
    },
}

/// Explicit byte budgets for a batched module query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleQueryOutputBudgets {
    pub per_field_bytes: u32,
    pub total_bytes: u32,
}

impl Default for ModuleQueryOutputBudgets {
    fn default() -> Self {
        Self {
            per_field_bytes: 8 * 1024,
            total_bytes: 64 * 1024,
        }
    }
}

/// One selector inside a batched module-processing request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleQuerySelector {
    Diagnostics {
        id: String,
    },
    ProofState {
        id: String,
        line: u32,
        column: u32,
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

impl ModuleQuerySelector {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Diagnostics { id }
            | Self::ProofState { id, .. }
            | Self::TypeAt { id, .. }
            | Self::References { id, .. }
            | Self::DeclarationTarget { id, .. }
            | Self::SurroundingDeclaration { id, .. } => id,
        }
    }
}

impl<'lean> IntoLean<'lean> for ModuleQuery {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Diagnostics => 0u8.into_lean(runtime),
            Self::TypeAt { line, column } => {
                alloc_ctor_with_objects(runtime, 1, [line.into_lean(runtime), column.into_lean(runtime)])
            }
            Self::GoalAt { line, column } => {
                alloc_ctor_with_objects(runtime, 2, [line.into_lean(runtime), column.into_lean(runtime)])
            }
            Self::References { name } => alloc_ctor_with_objects(runtime, 3, [name.into_lean(runtime)]),
        }
    }
}

impl<'lean> IntoLean<'lean> for ModuleQueryOutputBudgets {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.per_field_bytes.into_lean(runtime),
                self.total_bytes.into_lean(runtime),
            ],
        )
    }
}

impl sealed::SealedAbi for ModuleQueryOutputBudgets {}

impl<'lean> LeanAbi<'lean> for ModuleQueryOutputBudgets {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "ModuleQueryOutputBudgets cannot decode a Lean call result; it is an argument-only type",
        ))
    }
}

impl sealed::SealedAbi for &ModuleQueryOutputBudgets {}

impl<'lean> LeanAbi<'lean> for &ModuleQueryOutputBudgets {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.clone().into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "&ModuleQueryOutputBudgets cannot decode a Lean call result; use ModuleQueryOutputBudgets for owned values",
        ))
    }
}

impl<'lean> IntoLean<'lean> for ModuleQuerySelector {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Diagnostics { id } => alloc_ctor_with_objects(runtime, 0, [id.into_lean(runtime)]),
            Self::ProofState { id, line, column } => alloc_ctor_with_objects(
                runtime,
                1,
                [
                    id.into_lean(runtime),
                    line.into_lean(runtime),
                    column.into_lean(runtime),
                ],
            ),
            Self::TypeAt { id, line, column } => alloc_ctor_with_objects(
                runtime,
                2,
                [
                    id.into_lean(runtime),
                    line.into_lean(runtime),
                    column.into_lean(runtime),
                ],
            ),
            Self::References { id, name } => {
                alloc_ctor_with_objects(runtime, 3, [id.into_lean(runtime), name.into_lean(runtime)])
            }
            Self::DeclarationTarget { id, name, line, column } => alloc_ctor_with_objects(
                runtime,
                4,
                [
                    id.into_lean(runtime),
                    name.into_lean(runtime),
                    line.into_lean(runtime),
                    column.into_lean(runtime),
                ],
            ),
            Self::SurroundingDeclaration { id, line, column } => alloc_ctor_with_objects(
                runtime,
                5,
                [
                    id.into_lean(runtime),
                    line.into_lean(runtime),
                    column.into_lean(runtime),
                ],
            ),
        }
    }
}

impl<'lean> TryFromLean<'lean> for ModuleQuerySelector {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        drop(obj);
        Err(conversion_error(
            "ModuleQuerySelector cannot decode a Lean call result; it is an argument-only type",
        ))
    }
}

impl sealed::SealedAbi for ModuleQuerySelector {}

impl<'lean> LeanAbi<'lean> for ModuleQuerySelector {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "ModuleQuerySelector cannot decode a Lean call result; it is an argument-only type",
        ))
    }
}

impl sealed::SealedAbi for ModuleQuery {}

impl<'lean> LeanAbi<'lean> for ModuleQuery {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "ModuleQuery cannot decode a Lean call result; it is an argument-only type",
        ))
    }
}

impl sealed::SealedAbi for &ModuleQuery {}

impl<'lean> LeanAbi<'lean> for &ModuleQuery {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.clone().into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "&ModuleQuery cannot decode a Lean call result; use ModuleQuery for owned values",
        ))
    }
}

/// Source span in the original file. Positions are 1-based.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleSourceSpan {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

impl<'lean> TryFromLean<'lean> for ModuleSourceSpan {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [sl, sc, el, ec] = take_ctor_objects::<4>(obj, 0, "ModuleSourceSpan")?;
        Ok(Self {
            start_line: u32::try_from_lean(sl)?,
            start_column: u32::try_from_lean(sc)?,
            end_line: u32::try_from_lean(el)?,
            end_column: u32::try_from_lean(ec)?,
        })
    }
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

/// Source metadata for the declaration surrounding a proof-agent query.
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
}

impl<'lean> TryFromLean<'lean> for ProofStateInfo {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let truncated = bool_tail(&obj, 0, "ProofStateInfo.truncated")?;
        let [
            declaration_name,
            namespace_name,
            safe_edit,
            span,
            goals_before,
            goals_after,
            locals,
            expected_type,
        ] = take_ctor_objects::<8>(obj, 0, "ProofStateInfo")?;
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
        })
    }
}

/// Result for [`ModuleQuerySelector::ProofState`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProofStateResult {
    State(Box<ProofStateInfo>),
    Unavailable { message: String },
}

impl<'lean> TryFromLean<'lean> for ProofStateResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match sum_tag(&obj)? {
            0 => {
                let [info] = take_ctor_objects::<1>(obj, 0, "ProofStateResult::state")?;
                Ok(Self::State(Box::new(ProofStateInfo::try_from_lean(info)?)))
            }
            1 => {
                let [message] = take_ctor_objects::<1>(obj, 1, "ProofStateResult::unavailable")?;
                Ok(Self::Unavailable {
                    message: String::try_from_lean(message)?,
                })
            }
            other => Err(conversion_error(format!(
                "expected Lean ProofStateResult ctor (tag 0..=1), found tag {other}"
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
            other => Err(conversion_error(format!(
                "expected Lean ModuleQueryBatchResult ctor (tag 0..=5), found tag {other}"
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
