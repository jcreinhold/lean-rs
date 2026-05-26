//! Bounded module-query projections returned by
//! [`crate::LeanSession::process_module_query`].
//!
//! Callers choose a query shape; Lean owns module-header handling,
//! elaboration, info-tree traversal, cursor selection, and bounded
//! rendering. Rust decodes only the requested projection.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment; the blanket allow keeps the scope minimal per
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use lean_rs::abi::structure::{alloc_ctor_with_objects, ctor_tag, take_ctor_objects};
use lean_rs::abi::traits::{IntoLean, LeanAbi, TryFromLean, conversion_error, sealed};
use lean_rs::{LeanRuntime, Obj};
use lean_rs_sys::ctor::lean_ctor_get_uint8;
use lean_rs_sys::lean_object;
use lean_rs_sys::object::{lean_is_scalar, lean_unbox};

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

impl sealed::SealedAbi for ModuleQuery {}

impl<'lean> LeanAbi<'lean> for ModuleQuery {
    type CRepr = *mut lean_object;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }

    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
    )]
    fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        // SAFETY: `c` owns one Lean reference per Lake's `lean_obj_res`
        // contract; wrap-and-drop releases it on this unreachable decode path.
        drop(unsafe { Obj::from_owned_raw(runtime, c) });
        Err(conversion_error(
            "ModuleQuery cannot decode a Lean call result; it is an argument-only type",
        ))
    }
}

impl sealed::SealedAbi for &ModuleQuery {}

impl<'lean> LeanAbi<'lean> for &ModuleQuery {
    type CRepr = *mut lean_object;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.clone().into_lean(runtime).into_raw()
    }

    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait — caller invariant documented on LeanAbi::from_c"
    )]
    fn from_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        // SAFETY: see the owned `ModuleQuery` impl.
        drop(unsafe { Obj::from_owned_raw(runtime, c) });
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

fn bool_tail(obj: &Obj<'_>, offset: u32, label: &str) -> lean_rs::LeanResult<bool> {
    let tag = ctor_tag(obj)?;
    if tag != 0 {
        return Err(conversion_error(format!(
            "expected Lean {label} constructor tag 0, found tag {tag}"
        )));
    }
    let ptr = obj.as_raw_borrowed();
    // SAFETY: ctor tag validated above; callers pass the scalar-tail offset
    // for a Bool field in the corresponding Lean structure/constructor.
    match unsafe { lean_ctor_get_uint8(ptr, offset) } {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(conversion_error(format!(
            "Lean {label} byte {other} is not in {{0, 1}}"
        ))),
    }
}

fn sum_tag(obj: &Obj<'_>) -> lean_rs::LeanResult<u8> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: `lean_is_scalar` is pure pointer-bit inspection.
    if unsafe { lean_is_scalar(ptr) } {
        // SAFETY: scalar branch verified above.
        let tag = unsafe { lean_unbox(ptr) };
        return u8::try_from(tag)
            .map_err(|_| conversion_error(format!("Lean scalar constructor tag {tag} does not fit in u8")));
    }
    ctor_tag(obj)
}
