//! Structured declaration search bridge for the bundled host shim.
//!
//! Lean owns the environment scan because it can inspect `ConstantInfo`
//! without rendering types. Rust only encodes the request policy and decodes
//! bounded rows plus fanout facts.

use lean_rs::Obj;
use lean_rs::abi::nat;
use lean_rs::abi::structure::{take_ctor_objects, view};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
pub use lean_rs::host_bridge::declaration_requests::{
    DeclarationInspectionBudgets, DeclarationInspectionFields, DeclarationInspectionRequest, DeclarationNameMatch,
    DeclarationSearchRequest,
};

use crate::host::session::LeanSourceRange;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeclarationFlags {
    pub is_private: bool,
    pub is_generated: bool,
    pub is_internal: bool,
}

/// How a declaration's full name matched the request's name fragment.
///
/// The variants are in result order: search returns exact names first, then
/// prefixes, then substrings, so a limit keeps the closest names.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DeclarationNameMatchKind {
    /// The name is the fragment, compared with case.
    Exact,
    /// The name starts with the fragment, ignoring ASCII case.
    Prefix,
    /// The name contains the fragment after its start, ignoring ASCII case.
    Substring,
}

/// One declaration that a search kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSearchRow {
    pub name: String,
    /// The constant's kind, such as `definition` or `theorem`. A declaration
    /// with the instance attribute reports `instance`.
    pub kind: String,
    pub module: Option<String>,
    pub source: Option<LeanSourceRange>,
    pub match_reason: String,
    /// How the name matched the fragment; `None` when the request had no fragment.
    pub name_match: Option<DeclarationNameMatchKind>,
    pub flags: DeclarationFlags,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSearchPruning {
    pub stage: String,
    pub reason: String,
    pub count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclarationSearchTimings {
    pub scan_micros: u64,
    pub sort_micros: u64,
    pub source_micros: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeanDerivedWorkFacts {
    pub source_range_lookups: u64,
    pub docstring_lookups: u64,
    pub raw_type_renderings: u64,
    pub pretty_prints: u64,
    pub proof_search_fact_collections: u64,
    pub simp_extension_lookups: u64,
    pub parser_elaborator_runs: u64,
    pub module_snapshot_builds: u64,
    pub lazy_discr_tree_import_initialization_observed: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclarationSearchFacts {
    pub declarations_scanned: usize,
    pub after_name_filter: usize,
    pub after_kind_filter: usize,
    pub after_required_constants_filter: usize,
    pub after_conclusion_filter: usize,
    pub source_lookups: usize,
    pub broad_pruning: Vec<DeclarationSearchPruning>,
    pub truncated: bool,
    pub timings: DeclarationSearchTimings,
    pub derived_work: LeanDerivedWorkFacts,
}

/// The rows a search kept, in [`DeclarationNameMatchKind`] order and then by
/// name, cut to the request's limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSearchResult {
    pub declarations: Vec<DeclarationSearchRow>,
    pub truncated: bool,
    pub facts: DeclarationSearchFacts,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationRenderedInfo {
    pub value: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "proof-search booleans are independent inspection facts, not control-flow state"
)]
pub struct DeclarationProofSearchFacts {
    pub computed: bool,
    pub unavailable_reason: Option<String>,
    pub is_simp: bool,
    pub is_rw_candidate: bool,
    pub is_instance: bool,
    pub is_class: bool,
    pub class_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationInspection {
    pub name: String,
    pub kind: String,
    pub module: Option<String>,
    pub source: Option<LeanSourceRange>,
    pub statement: Option<DeclarationRenderedInfo>,
    pub docstring: Option<DeclarationRenderedInfo>,
    pub attributes: Vec<String>,
    pub proof_search: DeclarationProofSearchFacts,
    pub flags: DeclarationFlags,
    pub derived_work: LeanDerivedWorkFacts,
    /// Rendering that produced `statement`: `Some(true)` = pretty, `Some(false)`
    /// = raw, `None` when no statement was requested.
    pub statement_pretty: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclarationInspectionResult {
    Found { declaration: Box<DeclarationInspection> },
    NotFound { name: String },
    Unsupported,
}

fn bool_from_nat(obj: Obj<'_>) -> lean_rs::LeanResult<bool> {
    Ok(nat::try_to_usize(obj)? != 0)
}

impl<'lean> TryFromLean<'lean> for DeclarationFlags {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [is_private, is_generated, is_internal] = take_ctor_objects::<3>(obj, 0, "DeclarationFlags")?;
        Ok(Self {
            is_private: bool_from_nat(is_private)?,
            is_generated: bool_from_nat(is_generated)?,
            is_internal: bool_from_nat(is_internal)?,
        })
    }
}

// The shim encodes the match as a `Nat`: 0 for no fragment, then 1, 2, 3 in
// `DeclarationNameMatchKind` order.
fn name_match_from_nat(obj: Obj<'_>) -> lean_rs::LeanResult<Option<DeclarationNameMatchKind>> {
    match nat::try_to_usize(obj)? {
        0 => Ok(None),
        1 => Ok(Some(DeclarationNameMatchKind::Exact)),
        2 => Ok(Some(DeclarationNameMatchKind::Prefix)),
        3 => Ok(Some(DeclarationNameMatchKind::Substring)),
        other => Err(conversion_error(format!(
            "expected declaration name match 0..=3, found {other}"
        ))),
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationSearchRow {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [name, kind, module, source, match_reason, name_match, flags] =
            take_ctor_objects::<7>(obj, 0, "DeclarationSearchRow")?;
        Ok(Self {
            name: String::try_from_lean(name)?,
            kind: String::try_from_lean(kind)?,
            module: Option::<String>::try_from_lean(module)?,
            source: Option::<LeanSourceRange>::try_from_lean(source)?,
            match_reason: String::try_from_lean(match_reason)?,
            name_match: name_match_from_nat(name_match)?,
            flags: DeclarationFlags::try_from_lean(flags)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationSearchPruning {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [stage, reason, count] = take_ctor_objects::<3>(obj, 0, "DeclarationSearchPruning")?;
        Ok(Self {
            stage: String::try_from_lean(stage)?,
            reason: String::try_from_lean(reason)?,
            count: nat::try_to_usize(count)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationSearchTimings {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [scan_micros, sort_micros, source_micros] = take_ctor_objects::<3>(obj, 0, "DeclarationSearchTimings")?;
        Ok(Self {
            scan_micros: nat::try_to_u64(scan_micros)?,
            sort_micros: nat::try_to_u64(sort_micros)?,
            source_micros: nat::try_to_u64(source_micros)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for LeanDerivedWorkFacts {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [
            source_range_lookups,
            docstring_lookups,
            raw_type_renderings,
            pretty_prints,
            proof_search_fact_collections,
            simp_extension_lookups,
            parser_elaborator_runs,
            module_snapshot_builds,
            lazy_discr_tree_import_initialization_observed,
        ] = take_ctor_objects::<9>(obj, 0, "DerivedWorkFacts")?;
        Ok(Self {
            source_range_lookups: nat::try_to_u64(source_range_lookups)?,
            docstring_lookups: nat::try_to_u64(docstring_lookups)?,
            raw_type_renderings: nat::try_to_u64(raw_type_renderings)?,
            pretty_prints: nat::try_to_u64(pretty_prints)?,
            proof_search_fact_collections: nat::try_to_u64(proof_search_fact_collections)?,
            simp_extension_lookups: nat::try_to_u64(simp_extension_lookups)?,
            parser_elaborator_runs: nat::try_to_u64(parser_elaborator_runs)?,
            module_snapshot_builds: nat::try_to_u64(module_snapshot_builds)?,
            lazy_discr_tree_import_initialization_observed: bool_from_nat(
                lazy_discr_tree_import_initialization_observed,
            )?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationSearchFacts {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [
            declarations_scanned,
            after_name_filter,
            after_kind_filter,
            after_required_constants_filter,
            after_conclusion_filter,
            source_lookups,
            broad_pruning,
            truncated,
            timings,
            derived_work,
        ] = take_ctor_objects::<10>(obj, 0, "DeclarationSearchFacts")?;
        Ok(Self {
            declarations_scanned: nat::try_to_usize(declarations_scanned)?,
            after_name_filter: nat::try_to_usize(after_name_filter)?,
            after_kind_filter: nat::try_to_usize(after_kind_filter)?,
            after_required_constants_filter: nat::try_to_usize(after_required_constants_filter)?,
            after_conclusion_filter: nat::try_to_usize(after_conclusion_filter)?,
            source_lookups: nat::try_to_usize(source_lookups)?,
            broad_pruning: Vec::<DeclarationSearchPruning>::try_from_lean(broad_pruning)?,
            truncated: bool_from_nat(truncated)?,
            timings: DeclarationSearchTimings::try_from_lean(timings)?,
            derived_work: LeanDerivedWorkFacts::try_from_lean(derived_work)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationSearchResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [declarations, truncated, facts] = take_ctor_objects::<3>(obj, 0, "DeclarationSearchResult")?;
        Ok(Self {
            declarations: Vec::<DeclarationSearchRow>::try_from_lean(declarations)?,
            truncated: bool_from_nat(truncated)?,
            facts: DeclarationSearchFacts::try_from_lean(facts)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationRenderedInfo {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [value, truncated] = take_ctor_objects::<2>(obj, 0, "DeclarationRenderedInfo")?;
        Ok(Self {
            value: String::try_from_lean(value)?,
            truncated: bool_from_nat(truncated)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationProofSearchFacts {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [
            computed,
            unavailable_reason,
            is_simp,
            is_rw_candidate,
            is_instance,
            is_class,
            class_name,
        ] = take_ctor_objects::<7>(obj, 0, "DeclarationProofSearchFacts")?;
        Ok(Self {
            computed: bool_from_nat(computed)?,
            unavailable_reason: Option::<String>::try_from_lean(unavailable_reason)?,
            is_simp: bool_from_nat(is_simp)?,
            is_rw_candidate: bool_from_nat(is_rw_candidate)?,
            is_instance: bool_from_nat(is_instance)?,
            is_class: bool_from_nat(is_class)?,
            class_name: Option::<String>::try_from_lean(class_name)?,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationInspection {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [
            name,
            kind,
            module,
            source,
            statement,
            docstring,
            attributes,
            proof_search,
            flags,
            derived_work,
            statement_rendering,
        ] = take_ctor_objects::<11>(obj, 0, "DeclarationInspection")?;
        // `statementRendering : Option Nat` (1 = pretty, 0 = raw) → Option<bool>.
        let statement_pretty = match view(&statement_rendering).sum_tag()? {
            0 => None,
            1 => {
                let [nat] = take_ctor_objects::<1>(statement_rendering, 1, "DeclarationInspection.statementRendering")?;
                Some(bool_from_nat(nat)?)
            }
            other => {
                return Err(conversion_error(format!(
                    "expected Lean Option ctor (tag 0..=1) for statementRendering, found tag {other}"
                )));
            }
        };
        Ok(Self {
            name: String::try_from_lean(name)?,
            kind: String::try_from_lean(kind)?,
            module: Option::<String>::try_from_lean(module)?,
            source: Option::<LeanSourceRange>::try_from_lean(source)?,
            statement: Option::<DeclarationRenderedInfo>::try_from_lean(statement)?,
            docstring: Option::<DeclarationRenderedInfo>::try_from_lean(docstring)?,
            attributes: Vec::<String>::try_from_lean(attributes)?,
            proof_search: DeclarationProofSearchFacts::try_from_lean(proof_search)?,
            flags: DeclarationFlags::try_from_lean(flags)?,
            derived_work: LeanDerivedWorkFacts::try_from_lean(derived_work)?,
            statement_pretty,
        })
    }
}

impl<'lean> TryFromLean<'lean> for DeclarationInspectionResult {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        match view(&obj).sum_tag()? {
            0 => {
                let [declaration] = take_ctor_objects::<1>(obj, 0, "DeclarationInspectionResult::found")?;
                Ok(Self::Found {
                    declaration: Box::new(DeclarationInspection::try_from_lean(declaration)?),
                })
            }
            1 => {
                let [name] = take_ctor_objects::<1>(obj, 1, "DeclarationInspectionResult::notFound")?;
                Ok(Self::NotFound {
                    name: String::try_from_lean(name)?,
                })
            }
            2 => {
                let [] = take_ctor_objects::<0>(obj, 2, "DeclarationInspectionResult::unsupported")?;
                Ok(Self::Unsupported)
            }
            other => Err(conversion_error(format!(
                "expected Lean DeclarationInspectionResult ctor (tag 0..=2), found tag {other}"
            ))),
        }
    }
}
