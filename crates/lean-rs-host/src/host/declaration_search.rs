//! Structured declaration search bridge for the bundled host shim.
//!
//! Lean owns the environment scan because it can inspect `ConstantInfo`
//! without rendering types. Rust only encodes the request policy and decodes
//! bounded rows plus fanout facts.

use lean_rs::abi::nat;
use lean_rs::abi::structure::{alloc_ctor_with_objects, take_ctor_objects, view};
use lean_rs::abi::traits::{IntoLean, LeanAbi, TryFromLean, conversion_error, sealed};
use lean_rs::{LeanRuntime, Obj};
use lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX;

use crate::host::session::{LeanDeclarationFilter, LeanSourceRange};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeclarationNameMatch {
    #[default]
    Contains,
    Suffix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationSearchScope {
    Namespace,
    Module,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSearchBias {
    pub scope: DeclarationSearchScope,
    pub prefix: String,
    pub strict: bool,
    pub weight: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSearchRequest {
    pub name_fragment: Option<String>,
    pub name_match: DeclarationNameMatch,
    pub kind: Option<String>,
    pub required_constants: Vec<String>,
    pub conclusion_head: Option<String>,
    pub scope_biases: Vec<DeclarationSearchBias>,
    pub limit: usize,
    pub filter: LeanDeclarationFilter,
    pub include_source: bool,
}

impl DeclarationSearchRequest {
    #[must_use]
    pub fn new(name_fragment: impl Into<String>) -> Self {
        Self {
            name_fragment: Some(name_fragment.into()),
            name_match: DeclarationNameMatch::Contains,
            kind: None,
            required_constants: Vec::new(),
            conclusion_head: None,
            scope_biases: Vec::new(),
            limit: 20,
            filter: LeanDeclarationFilter {
                include_private: false,
                include_generated: false,
                include_internal: false,
            },
            include_source: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeclarationFlags {
    pub is_private: bool,
    pub is_generated: bool,
    pub is_internal: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSearchRow {
    pub name: String,
    pub kind: String,
    pub module: Option<String>,
    pub source: Option<LeanSourceRange>,
    pub match_reason: String,
    pub score: i32,
    pub rank: usize,
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
    pub rank_micros: u64,
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
    pub after_scope_filter: usize,
    pub source_lookups: usize,
    pub broad_pruning: Vec<DeclarationSearchPruning>,
    pub truncated: bool,
    pub timings: DeclarationSearchTimings,
    pub derived_work: LeanDerivedWorkFacts,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSearchResult {
    pub declarations: Vec<DeclarationSearchRow>,
    pub truncated: bool,
    pub facts: DeclarationSearchFacts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "field-selection flags mirror the Lean request shape and are clearer than five tiny enums"
)]
pub struct DeclarationInspectionFields {
    pub source: bool,
    pub statement: bool,
    pub docstring: bool,
    pub attributes: bool,
    pub flags: bool,
    /// Render `statement` notation-aware (`pp.universes false`) when `true`,
    /// falling back to the raw term if the pretty-printer cannot render it;
    /// the fully-elaborated raw form when `false`.
    pub statement_pretty: bool,
    /// Include proof-search-oriented facts such as simp/rw/instance/class.
    /// Defaults off because these facts may touch persistent extensions and
    /// lazy derived search indexes.
    pub proof_search: bool,
}

impl Default for DeclarationInspectionFields {
    fn default() -> Self {
        Self {
            source: true,
            statement: true,
            docstring: true,
            attributes: true,
            flags: true,
            statement_pretty: true,
            proof_search: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationInspectionBudgets {
    /// Maximum UTF-8 bytes for one rendered field.
    pub per_field_bytes: u32,
    /// Maximum UTF-8 bytes for all rendered fields in the inspection.
    pub total_bytes: u32,
}

impl Default for DeclarationInspectionBudgets {
    fn default() -> Self {
        Self {
            per_field_bytes: 8 * 1024,
            total_bytes: 64 * 1024,
        }
    }
}

impl DeclarationInspectionBudgets {
    /// Construct the default inspection budget bundle.
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

    /// Replace the total inspection byte budget, saturating at
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationInspectionRequest {
    pub name: String,
    pub fields: DeclarationInspectionFields,
    pub budgets: DeclarationInspectionBudgets,
}

impl DeclarationInspectionRequest {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: DeclarationInspectionFields::default(),
            budgets: DeclarationInspectionBudgets::default(),
        }
    }
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

impl<'lean> IntoLean<'lean> for DeclarationNameMatch {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Contains => nat::from_usize(runtime, 0),
            Self::Suffix => nat::from_usize(runtime, 1),
        }
    }
}

impl<'lean> IntoLean<'lean> for DeclarationSearchScope {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        match self {
            Self::Namespace => nat::from_usize(runtime, 0),
            Self::Module => nat::from_usize(runtime, 1),
        }
    }
}

fn nat_from_bool(runtime: &LeanRuntime, value: bool) -> Obj<'_> {
    nat::from_usize(runtime, usize::from(value))
}

fn bool_from_nat(obj: Obj<'_>) -> lean_rs::LeanResult<bool> {
    Ok(nat::try_to_usize(obj)? != 0)
}

impl<'lean> IntoLean<'lean> for DeclarationSearchBias {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.scope.into_lean(runtime),
                self.prefix.into_lean(runtime),
                nat_from_bool(runtime, self.strict),
                self.weight.to_string().into_lean(runtime),
            ],
        )
    }
}

impl<'lean> IntoLean<'lean> for DeclarationSearchRequest {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.name_fragment.into_lean(runtime),
                self.name_match.into_lean(runtime),
                self.kind.into_lean(runtime),
                self.required_constants.into_lean(runtime),
                self.conclusion_head.into_lean(runtime),
                self.scope_biases.into_lean(runtime),
                nat::from_usize(runtime, self.limit),
                self.filter.into_lean(runtime),
                nat_from_bool(runtime, self.include_source),
            ],
        )
    }
}

impl<'lean> IntoLean<'lean> for DeclarationInspectionFields {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                nat_from_bool(runtime, self.source),
                nat_from_bool(runtime, self.statement),
                nat_from_bool(runtime, self.docstring),
                nat_from_bool(runtime, self.attributes),
                nat_from_bool(runtime, self.flags),
                nat_from_bool(runtime, self.statement_pretty),
                nat_from_bool(runtime, self.proof_search),
            ],
        )
    }
}

impl<'lean> IntoLean<'lean> for DeclarationInspectionBudgets {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        let normalized = self.normalized();
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                nat::from_usize(runtime, normalized.per_field_bytes as usize),
                nat::from_usize(runtime, normalized.total_bytes as usize),
            ],
        )
    }
}

impl<'lean> IntoLean<'lean> for DeclarationInspectionRequest {
    fn into_lean(self, runtime: &'lean LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.name.into_lean(runtime),
                self.fields.into_lean(runtime),
                self.budgets.into_lean(runtime),
            ],
        )
    }
}

impl sealed::SealedAbi for DeclarationInspectionRequest {}

impl<'lean> LeanAbi<'lean> for DeclarationInspectionRequest {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "DeclarationInspectionRequest cannot decode a Lean call result; it is an argument-only type",
        ))
    }
}

impl sealed::SealedAbi for &DeclarationInspectionRequest {}

impl<'lean> LeanAbi<'lean> for &DeclarationInspectionRequest {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.clone().into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "&DeclarationInspectionRequest cannot decode a Lean call result; use DeclarationInspectionRequest for owned values",
        ))
    }
}

impl sealed::SealedAbi for DeclarationSearchRequest {}

impl<'lean> LeanAbi<'lean> for DeclarationSearchRequest {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "DeclarationSearchRequest cannot decode a Lean call result; it is an argument-only type",
        ))
    }
}

impl sealed::SealedAbi for &DeclarationSearchRequest {}

impl<'lean> LeanAbi<'lean> for &DeclarationSearchRequest {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;

    fn into_c(self, runtime: &'lean LeanRuntime) -> Self::CRepr {
        self.clone().into_lean(runtime).into_raw()
    }

    fn from_c(_c: Self::CRepr, _runtime: &'lean LeanRuntime) -> lean_rs::LeanResult<Self> {
        Err(conversion_error(
            "&DeclarationSearchRequest cannot decode a Lean call result; use DeclarationSearchRequest for owned values",
        ))
    }
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

impl<'lean> TryFromLean<'lean> for DeclarationSearchRow {
    fn try_from_lean(obj: Obj<'lean>) -> lean_rs::LeanResult<Self> {
        let [name, kind, module, source, match_reason, score, rank, flags] =
            take_ctor_objects::<8>(obj, 0, "DeclarationSearchRow")?;
        Ok(Self {
            name: String::try_from_lean(name)?,
            kind: String::try_from_lean(kind)?,
            module: Option::<String>::try_from_lean(module)?,
            source: Option::<LeanSourceRange>::try_from_lean(source)?,
            match_reason: String::try_from_lean(match_reason)?,
            score: String::try_from_lean(score)?
                .parse()
                .map_err(|_| conversion_error("score does not fit i32"))?,
            rank: nat::try_to_usize(rank)?,
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
        let [scan_micros, rank_micros, source_micros] = take_ctor_objects::<3>(obj, 0, "DeclarationSearchTimings")?;
        Ok(Self {
            scan_micros: nat::try_to_u64(scan_micros)?,
            rank_micros: nat::try_to_u64(rank_micros)?,
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
            after_scope_filter,
            source_lookups,
            broad_pruning,
            truncated,
            timings,
            derived_work,
        ] = take_ctor_objects::<11>(obj, 0, "DeclarationSearchFacts")?;
        Ok(Self {
            declarations_scanned: nat::try_to_usize(declarations_scanned)?,
            after_name_filter: nat::try_to_usize(after_name_filter)?,
            after_kind_filter: nat::try_to_usize(after_kind_filter)?,
            after_required_constants_filter: nat::try_to_usize(after_required_constants_filter)?,
            after_conclusion_filter: nat::try_to_usize(after_conclusion_filter)?,
            after_scope_filter: nat::try_to_usize(after_scope_filter)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_inspection_budget_defaults_match_policy() {
        let budgets = DeclarationInspectionBudgets::new();
        assert_eq!(budgets.per_field_bytes, 8 * 1024);
        assert_eq!(budgets.total_bytes, 64 * 1024);
    }

    #[test]
    fn declaration_inspection_budget_setters_saturate() {
        let budgets = DeclarationInspectionBudgets::new()
            .per_field_bytes(u32::MAX)
            .total_bytes(u32::MAX);
        let max = clamp_output_budget(u32::MAX);
        assert_eq!(budgets.per_field_bytes, max);
        assert_eq!(budgets.total_bytes, max);
    }

    #[test]
    fn declaration_inspection_budget_normalization_clamps_struct_literals() {
        let budgets = DeclarationInspectionBudgets {
            per_field_bytes: u32::MAX,
            total_bytes: u32::MAX,
        }
        .normalized();
        let max = clamp_output_budget(u32::MAX);
        assert_eq!(budgets.per_field_bytes, max);
        assert_eq!(budgets.total_bytes, max);
    }
}
