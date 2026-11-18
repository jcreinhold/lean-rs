use crate::abi::structure::{alloc_ctor_with_objects, take_ctor_objects};
use crate::abi::traits::{IntoLean, LeanAbi, TryFromLean};
use crate::{LeanResult, Obj};

/// Lean-side declaration-listing filter.
///
/// The default is tuned for user-facing declaration browsers: include
/// private names because callers may be indexing the current project,
/// but drop compiler-generated and internal-detail names that usually
/// swamp the list with implementation artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeanDeclarationFilter {
    /// Keep names Lean marks as private.
    pub include_private: bool,
    /// Keep generated names with numeric components.
    pub include_generated: bool,
    /// Keep Lean internal-detail names such as `_`, `match_`, `proof_`,
    /// and similar implementation artifacts.
    pub include_internal: bool,
}

impl Default for LeanDeclarationFilter {
    fn default() -> Self {
        Self {
            include_private: true,
            include_generated: false,
            include_internal: false,
        }
    }
}

impl<'lean> IntoLean<'lean> for LeanDeclarationFilter {
    fn into_lean(self, runtime: &'lean crate::LeanRuntime) -> Obj<'lean> {
        alloc_ctor_with_objects(
            runtime,
            0,
            [
                self.include_private.into_lean(runtime),
                self.include_generated.into_lean(runtime),
                self.include_internal.into_lean(runtime),
            ],
        )
    }
}

impl<'lean> TryFromLean<'lean> for LeanDeclarationFilter {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [include_private_o, include_generated_o, include_internal_o] =
            take_ctor_objects::<3>(obj, 0, "DeclarationFilter")?;
        Ok(Self {
            include_private: bool::try_from_lean(include_private_o)?,
            include_generated: bool::try_from_lean(include_generated_o)?,
            include_internal: bool::try_from_lean(include_internal_o)?,
        })
    }
}

// SAFETY: DeclarationFilter is a constructor with three owned Nat fields. Only
// zero and one are emitted, using Lean's scalar representations for these Nats.
#[allow(unsafe_code, reason = "fixed boxed layout for the bundled declaration filter")]
unsafe impl<'lean> LeanAbi<'lean> for LeanDeclarationFilter {
    type CRepr = <Obj<'lean> as LeanAbi<'lean>>::CRepr;
    fn into_c(self, runtime: &'lean crate::LeanRuntime) -> Self::CRepr {
        self.into_lean(runtime).into_raw()
    }
    unsafe fn from_c(value: Self::CRepr, runtime: &'lean crate::LeanRuntime) -> LeanResult<Self> {
        // SAFETY: the caller transfers an owned, initialized DeclarationFilter.
        Self::try_from_lean(unsafe { Obj::from_c(value, runtime) }?)
    }
}
