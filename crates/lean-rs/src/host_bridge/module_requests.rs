//! Owned request layouts for the bundled shim.
use crate::abi::structure::alloc_ctor_with_objects;
use crate::abi::structure::{take_ctor_objects, view};
use crate::abi::traits::IntoLean;
use crate::abi::traits::{TryFromLean, conversion_error};
use crate::{LeanRuntime, Obj};
use lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX;
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
///
/// The default is 8 KiB per rendered field and 64 KiB for the combined
/// selector payload. Setters and ABI encoding clamp oversized values at
/// [`LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`], so direct struct literals cannot send an
/// unbounded render request into Lean.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleQueryOutputBudgets {
    /// Maximum UTF-8 bytes for one rendered field.
    pub per_field_bytes: u32,
    /// Maximum estimated UTF-8 bytes for all selector results in the batch.
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

impl ModuleQueryOutputBudgets {
    /// Construct the default output budget bundle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the per-field byte budget, saturating at
    /// [`LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`].
    #[must_use]
    pub fn per_field_bytes(mut self, bytes: u32) -> Self {
        self.per_field_bytes = clamp_output_budget(bytes);
        self
    }

    /// Replace the total batch byte budget, saturating at
    /// [`LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX`].
    #[must_use]
    pub fn total_bytes(mut self, bytes: u32) -> Self {
        self.total_bytes = clamp_output_budget(bytes);
        self
    }

    fn normalized(self) -> Self {
        Self {
            per_field_bytes: clamp_output_budget(self.per_field_bytes),
            total_bytes: clamp_output_budget(self.total_bytes),
        }
    }
}

fn clamp_output_budget(bytes: u32) -> u32 {
    let max = u32::try_from(LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX).unwrap_or(u32::MAX);
    bytes.min(max)
}

/// Intent selector for one proof position inside a declaration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ProofPositionSelector {
    /// The first tactic state — the point after the first tactic has run.
    #[default]
    Default,
    /// The `index`-th tactic state — after the index-th tactic has run.
    Index { index: u32 },
    /// The tactic whose source text exactly matches `text`.
    AfterText { text: String, occurrence: Option<u32> },
    /// The goal state before any tactic runs — the pristine entry goal. A proof
    /// attempt splices its candidate before the first tactic, so a from-scratch
    /// tactic block elaborates against this goal.
    Entry,
}

/// Target for a non-mutating proof attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProofEditTarget {
    Declaration {
        name: String,
        position: ProofPositionSelector,
    },
}

/// One proof candidate to splice into an in-memory overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofCandidate {
    pub id: String,
    pub text: String,
}

/// Bounded request to try proof snippets without mutating source files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofAttemptRequest {
    pub source: String,
    pub edit: ProofEditTarget,
    pub candidates: Vec<ProofCandidate>,
    pub budgets: ModuleQueryOutputBudgets,
}

/// Target declaration for verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclarationVerificationTarget {
    Name { name: String },
    Span { span: ModuleSourceSpan },
}

/// Policy for `sorry`-like constructs during declaration verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SorryPolicy {
    Allow,
    Deny,
}

/// Bounded request to verify one declaration in a source snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationVerificationRequest {
    pub source: String,
    pub target: DeclarationVerificationTarget,
    pub sorry_policy: SorryPolicy,
    pub report_axioms: bool,
    pub budgets: ModuleQueryOutputBudgets,
}

/// One target inside a batch declaration-verification request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationVerificationBatchItem {
    pub id: String,
    pub target: DeclarationVerificationTarget,
}

/// Bounded request to verify several declarations in one source snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationVerificationBatchRequest {
    pub source: String,
    pub targets: Vec<DeclarationVerificationBatchItem>,
    pub sorry_policy: SorryPolicy,
    pub report_axioms: bool,
    pub budgets: ModuleQueryOutputBudgets,
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
    ProofStateInDeclaration {
        id: String,
        declaration: String,
        position: ProofPositionSelector,
        /// Render local hypotheses as raw, fully-elaborated `Expr` text rather
        /// than the default notation-aware delaboration. Expert opt-out.
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
    DeclarationOutline {
        id: String,
    },
}

impl ModuleQuerySelector {
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
            Self::DeclarationOutline { id } => id,
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
        let normalized = self.normalized();
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                normalized.per_field_bytes.into_lean(runtime),
                normalized.total_bytes.into_lean(runtime),
            ],
        )
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
            Self::ProofStateInDeclaration {
                id,
                declaration,
                position,
                locals_raw,
            } => alloc_ctor_with_objects(
                runtime,
                6,
                [
                    id.into_lean(runtime),
                    declaration.into_lean(runtime),
                    position.into_lean(runtime),
                    u32::from(locals_raw).into_lean(runtime),
                ],
            ),
            Self::DeclarationOutline { id } => alloc_ctor_with_objects(runtime, 7, [id.into_lean(runtime)]),
        }
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
    fn try_from_lean(obj: Obj<'lean>) -> crate::LeanResult<Self> {
        let [sl, sc, el, ec] = take_ctor_objects::<4>(obj, 0, "ModuleSourceSpan")?;
        Ok(Self {
            start_line: u32::try_from_lean(sl)?,
            start_column: u32::try_from_lean(sc)?,
            end_line: u32::try_from_lean(el)?,
            end_column: u32::try_from_lean(ec)?,
        })
    }
}

impl<'lean> IntoLean<'lean> for ModuleSourceSpan {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.start_line.into_lean(runtime),
                self.start_column.into_lean(runtime),
                self.end_line.into_lean(runtime),
                self.end_column.into_lean(runtime),
            ],
        )
    }
}

impl<'lean> IntoLean<'lean> for ProofPositionSelector {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Default => 0u8.into_lean(runtime),
            Self::Index { index } => alloc_ctor_with_objects(runtime, 1, [index.into_lean(runtime)]),
            Self::AfterText { text, occurrence } => {
                alloc_ctor_with_objects(runtime, 2, [text.into_lean(runtime), occurrence.into_lean(runtime)])
            }
            Self::Entry => 3u8.into_lean(runtime),
        }
    }
}

impl<'lean> IntoLean<'lean> for ProofEditTarget {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Declaration { name, position } => {
                alloc_ctor_with_objects(runtime, 0, [name.into_lean(runtime), position.into_lean(runtime)])
            }
        }
    }
}

impl<'lean> IntoLean<'lean> for ProofCandidate {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(runtime, 0, [self.id.into_lean(runtime), self.text.into_lean(runtime)])
    }
}

impl<'lean> IntoLean<'lean> for ProofAttemptRequest {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.source.into_lean(runtime),
                self.edit.into_lean(runtime),
                self.candidates.into_lean(runtime),
                self.budgets.into_lean(runtime),
            ],
        )
    }
}

impl<'lean> IntoLean<'lean> for DeclarationVerificationTarget {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Name { name } => alloc_ctor_with_objects(runtime, 0, [name.into_lean(runtime)]),
            Self::Span { span } => alloc_ctor_with_objects(runtime, 1, [span.into_lean(runtime)]),
        }
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationVerificationTarget {
    fn try_from_lean(obj: Obj<'lean>) -> crate::LeanResult<Self> {
        match view(&obj).sum_tag()? {
            0 => {
                let [name] = take_ctor_objects::<1>(obj, 0, "DeclarationVerificationTarget::name")?;
                Ok(Self::Name {
                    name: String::try_from_lean(name)?,
                })
            }
            1 => {
                let [span] = take_ctor_objects::<1>(obj, 1, "DeclarationVerificationTarget::span")?;
                Ok(Self::Span {
                    span: ModuleSourceSpan::try_from_lean(span)?,
                })
            }
            other => Err(conversion_error(format!(
                "expected Lean DeclarationVerificationTarget ctor (tag 0..=1), found tag {other}"
            ))),
        }
    }
}

impl<'lean> IntoLean<'lean> for SorryPolicy {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Allow => 0u32.into_lean(runtime),
            Self::Deny => 1u32.into_lean(runtime),
        }
    }
}

impl<'lean> IntoLean<'lean> for DeclarationVerificationRequest {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.source.into_lean(runtime),
                self.target.into_lean(runtime),
                self.sorry_policy.into_lean(runtime),
                (u32::from(self.report_axioms)).into_lean(runtime),
                self.budgets.into_lean(runtime),
            ],
        )
    }
}

impl<'lean> IntoLean<'lean> for DeclarationVerificationBatchItem {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(runtime, 0, [self.id.into_lean(runtime), self.target.into_lean(runtime)])
    }
}

impl<'lean> IntoLean<'lean> for DeclarationVerificationBatchRequest {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.source.into_lean(runtime),
                self.targets.into_lean(runtime),
                self.sorry_policy.into_lean(runtime),
                (u32::from(self.report_axioms)).into_lean(runtime),
                self.budgets.into_lean(runtime),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_query_output_budget_defaults_match_policy() {
        let budgets = ModuleQueryOutputBudgets::new();
        assert_eq!(budgets.per_field_bytes, 8 * 1024);
        assert_eq!(budgets.total_bytes, 64 * 1024);
    }

    #[test]
    fn module_query_output_budget_setters_saturate() {
        let budgets = ModuleQueryOutputBudgets::new()
            .per_field_bytes(u32::MAX)
            .total_bytes(u32::MAX);
        let max = clamp_output_budget(u32::MAX);
        assert_eq!(budgets.per_field_bytes, max);
        assert_eq!(budgets.total_bytes, max);
    }

    #[test]
    fn module_query_output_budget_normalization_clamps_struct_literals() {
        let budgets = ModuleQueryOutputBudgets {
            per_field_bytes: u32::MAX,
            total_bytes: u32::MAX,
        }
        .normalized();
        let max = clamp_output_budget(u32::MAX);
        assert_eq!(budgets.per_field_bytes, max);
        assert_eq!(budgets.total_bytes, max);
    }
}
