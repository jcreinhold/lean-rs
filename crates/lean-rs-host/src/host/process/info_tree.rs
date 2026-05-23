//! [`ProcessedFile`] and its four node types — the FFI-safe projection
//! of `Lean.Elab.InfoTree` returned by
//! [`crate::LeanSession::process_with_info_tree`].
//!
//! All fields are public: the projection is a value type, not a handle,
//! and the encoding is part of the contract with Lean (see
//! `LeanRsHostShims/InfoTree.lean`). Owned `String`s and primitive
//! integers only — the whole structure is `Send + Sync + 'static` so
//! it crosses worker-thread channels cleanly.

// SAFETY DOC: the single `unsafe { ... }` block in this file carries
// its own `// SAFETY:` comment; the blanket allow keeps the scope
// minimal per `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use lean_rs::Obj;
use lean_rs::abi::structure::{ctor_tag, take_ctor_objects};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
use lean_rs::error::LeanResult;
use lean_rs_sys::ctor::lean_ctor_get_uint8;

use crate::host::elaboration::LeanElabFailure;

/// One `Lean.Elab.TermInfo` projection.
///
/// `expr_str` / `type_str` use Lean's raw `Expr.toString` projection
/// (same shape as [`crate::LeanSession::expr_to_string_raw`]); for the
/// notation-aware rendering call the optional
/// [`crate::host::meta::pp_expr`] service against the original `Expr`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TermInfoNode {
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column.
    pub start_column: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_column: u32,
    /// `Expr.toString` of the elaborated expression.
    pub expr_str: String,
    /// `Expr.toString` of the inferred type, or empty if inference
    /// failed at the recorded site.
    pub type_str: String,
    /// `Expr.toString` of the expected type when the elaborator recorded
    /// one (e.g., a coercion site).
    pub expected_type_str: Option<String>,
}

impl<'lean> TryFromLean<'lean> for TermInfoNode {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [sl, sc, el, ec, expr, ty, exp_ty] = take_ctor_objects::<7>(obj, 0, "TermInfoNode")?;
        Ok(Self {
            start_line: u32::try_from_lean(sl)?,
            start_column: u32::try_from_lean(sc)?,
            end_line: u32::try_from_lean(el)?,
            end_column: u32::try_from_lean(ec)?,
            expr_str: String::try_from_lean(expr)?,
            type_str: String::try_from_lean(ty)?,
            expected_type_str: Option::<String>::try_from_lean(exp_ty)?,
        })
    }
}

/// One `Lean.Elab.TacticInfo` projection.
///
/// `goals_before` / `goals_after` are already pretty-printed by the
/// Lean shim inside the elaboration's `MetaM` context. Empty arrays
/// mean the elaborator recorded the tactic without an enclosing goals
/// list (rare). The strings carry no metavariable identity that the
/// Rust side can reuse — they are diagnostic text only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TacticInfoNode {
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column.
    pub start_column: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_column: u32,
    /// Goals as the user would see them before this tactic ran.
    pub goals_before: Vec<String>,
    /// Goals as the user would see them after this tactic ran.
    pub goals_after: Vec<String>,
}

impl<'lean> TryFromLean<'lean> for TacticInfoNode {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [sl, sc, el, ec, gb, ga] = take_ctor_objects::<6>(obj, 0, "TacticInfoNode")?;
        Ok(Self {
            start_line: u32::try_from_lean(sl)?,
            start_column: u32::try_from_lean(sc)?,
            end_line: u32::try_from_lean(el)?,
            end_column: u32::try_from_lean(ec)?,
            goals_before: Vec::<String>::try_from_lean(gb)?,
            goals_after: Vec::<String>::try_from_lean(ga)?,
        })
    }
}

/// One identifier occurrence the elaborator recorded.
///
/// `is_binder` distinguishes binding sites from use sites — the same
/// distinction Lean's LSP uses to answer "go to definition" vs. "find
/// references". `name` is the fully-qualified name when the reference
/// resolved to a constant; for binder occurrences it is the raw bound
/// identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameRefNode {
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column.
    pub start_column: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_column: u32,
    /// Fully-qualified name when resolved to a constant; raw identifier
    /// for binder occurrences.
    pub name: String,
    /// `true` for binding sites, `false` for use-site references.
    pub is_binder: bool,
}

impl<'lean> TryFromLean<'lean> for NameRefNode {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        // Lean packs the `Bool isBinder` field into the scalar tail of
        // the constructor (one byte at offset 0 past the five object
        // slots), mirroring `ElabFailure.truncated`. Read the scalar
        // before consuming the object slots.
        let tag = ctor_tag(&obj)?;
        if tag != 0 {
            return Err(conversion_error(format!(
                "expected Lean NameRefNode ctor (tag 0), found tag {tag}"
            )));
        }
        let ptr = obj.as_raw_borrowed();
        // SAFETY: ctor validated above; the first scalar-tail byte holds
        // the `Bool isBinder` value (0 = false, 1 = true).
        let is_binder_byte = unsafe { lean_ctor_get_uint8(ptr, 0) };
        let is_binder = match is_binder_byte {
            0 => false,
            1 => true,
            other => {
                return Err(conversion_error(format!(
                    "Lean NameRefNode.isBinder byte {other} is not in {{0, 1}}"
                )));
            }
        };
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

/// One top-level command the elaborator processed.
///
/// `decl_name` is set only for commands that introduce a named
/// declaration (`def`, `theorem`, `instance`, …); other commands such
/// as `#check` carry `None`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandInfoNode {
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column.
    pub start_column: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_column: u32,
    /// Declared name when the command introduces one.
    pub decl_name: Option<String>,
}

impl<'lean> TryFromLean<'lean> for CommandInfoNode {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [sl, sc, el, ec, dn] = take_ctor_objects::<5>(obj, 0, "CommandInfoNode")?;
        Ok(Self {
            start_line: u32::try_from_lean(sl)?,
            start_column: u32::try_from_lean(sc)?,
            end_line: u32::try_from_lean(el)?,
            end_column: u32::try_from_lean(ec)?,
            decl_name: Option::<String>::try_from_lean(dn)?,
        })
    }
}

/// FFI-safe projection of a processed Lean source string's
/// `Elab.InfoTree`.
///
/// Four arrays of structurally distinct nodes plus the diagnostics
/// payload the elaborator emitted. The projection is a value type:
/// no Lean references survive on the Rust side, so a `ProcessedFile`
/// crosses thread boundaries cleanly.
#[derive(Clone, Debug)]
pub struct ProcessedFile {
    /// One entry per top-level command.
    pub commands: Vec<CommandInfoNode>,
    /// One entry per `Elab.TermInfo` node the elaborator emitted.
    pub terms: Vec<TermInfoNode>,
    /// One entry per `Elab.TacticInfo` node the elaborator emitted.
    pub tactics: Vec<TacticInfoNode>,
    /// Every identifier occurrence (binding sites and use sites).
    pub names: Vec<NameRefNode>,
    /// Diagnostics from the elaborator's `MessageLog`, with the same
    /// byte-budget semantics as
    /// [`crate::host::evidence::LeanKernelOutcome::Rejected`].
    pub diagnostics: LeanElabFailure,
}

impl ProcessedFile {
    /// Return the innermost [`TermInfoNode`] whose source range contains
    /// the position `(line, column)`. `None` if no recorded term covers
    /// the position. Ties on range area are broken by encounter order
    /// (the elaborator's outer-to-inner traversal).
    #[must_use]
    pub fn term_at(&self, line: u32, column: u32) -> Option<&TermInfoNode> {
        smallest_containing(&self.terms, line, column, |n| {
            (n.start_line, n.start_column, n.end_line, n.end_column)
        })
    }

    /// Return the innermost [`TacticInfoNode`] whose source range
    /// contains the position.
    #[must_use]
    pub fn tactic_at(&self, line: u32, column: u32) -> Option<&TacticInfoNode> {
        smallest_containing(&self.tactics, line, column, |n| {
            (n.start_line, n.start_column, n.end_line, n.end_column)
        })
    }

    /// Return every [`NameRefNode`] whose `name` exactly matches `name`,
    /// in encounter order. No normalisation: the caller is responsible
    /// for matching the fully-qualified form Lean records (e.g.,
    /// `"Nat.succ"`, not `"succ"`).
    #[must_use]
    pub fn references_of<'a>(&'a self, name: &str) -> Vec<&'a NameRefNode> {
        self.names.iter().filter(|n| n.name == name).collect()
    }
}

impl<'lean> TryFromLean<'lean> for ProcessedFile {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [commands, terms, tactics, names, diagnostics] = take_ctor_objects::<5>(obj, 0, "ProcessedFile")?;
        Ok(Self {
            commands: Vec::<CommandInfoNode>::try_from_lean(commands)?,
            terms: Vec::<TermInfoNode>::try_from_lean(terms)?,
            tactics: Vec::<TacticInfoNode>::try_from_lean(tactics)?,
            names: Vec::<NameRefNode>::try_from_lean(names)?,
            diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
        })
    }
}

fn smallest_containing<T>(
    nodes: &[T],
    line: u32,
    column: u32,
    range: impl Fn(&T) -> (u32, u32, u32, u32),
) -> Option<&T> {
    let mut best: Option<(&T, u64)> = None;
    for node in nodes {
        let (sl, sc, el, ec) = range(node);
        if !range_contains(sl, sc, el, ec, line, column) {
            continue;
        }
        let area = range_area(sl, sc, el, ec);
        match best {
            None => best = Some((node, area)),
            Some((_, best_area)) if area < best_area => best = Some((node, area)),
            _ => {}
        }
    }
    best.map(|(node, _)| node)
}

fn range_contains(sl: u32, sc: u32, el: u32, ec: u32, line: u32, column: u32) -> bool {
    if line < sl || line > el {
        return false;
    }
    if line == sl && column < sc {
        return false;
    }
    if line == el && column > ec {
        return false;
    }
    true
}

fn range_area(sl: u32, sc: u32, el: u32, ec: u32) -> u64 {
    // Treat lines as worth a large constant so cross-line ranges always
    // dominate single-line ranges. Columns break ties on the same line.
    let line_span = u64::from(el.saturating_sub(sl));
    let col_span = u64::from(ec).saturating_sub(u64::from(sc));
    line_span.saturating_mul(1_000_000).saturating_add(col_span)
}
