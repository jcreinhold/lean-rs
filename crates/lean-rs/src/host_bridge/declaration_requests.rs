//! Owned request layouts for the bundled shim.
use super::LeanDeclarationFilter;
use crate::abi::nat;
use crate::abi::structure::alloc_ctor_with_objects;
use crate::abi::traits::IntoLean;
use crate::{LeanRuntime, Obj};
use lean_toolchain::LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX;
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
