//! Structured declaration search bridge for the bundled host shim.
//!
//! Lean owns the environment scan because it can inspect `ConstantInfo`
//! without rendering types. Rust only encodes the request policy and decodes
//! bounded rows plus fanout facts.

use lean_rs::abi::nat;
use lean_rs::abi::structure::{alloc_ctor_with_objects, take_ctor_objects};
use lean_rs::abi::traits::{IntoLean, LeanAbi, TryFromLean, conversion_error, sealed};
use lean_rs::{LeanRuntime, Obj};

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSearchResult {
    pub declarations: Vec<DeclarationSearchRow>,
    pub truncated: bool,
    pub facts: DeclarationSearchFacts,
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
        ] = take_ctor_objects::<10>(obj, 0, "DeclarationSearchFacts")?;
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
