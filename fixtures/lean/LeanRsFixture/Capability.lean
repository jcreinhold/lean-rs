import Lean

/-! ABI category: capability-monad declarations that are *not* `@[export]`-ed.
    `MetaM` and `CoreM` have no meaningful C ABI; they exist here so that the
    fixture build pulls in the Lean compiler imports and so later prompts can
    wrap these actions in `IO` without re-discovering the import surface. -/

namespace LeanRsFixture.Capability

open Lean Meta Core

/-- A trivial `CoreM` action; compiled but never called from this package. -/
def coreNoop : CoreM Unit := pure ()

/-- A trivial `MetaM` action returning the constant expression `True`. -/
def metaTrueConst : MetaM Expr := pure (.const ``True [])

end LeanRsFixture.Capability
