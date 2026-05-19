import Lean

/-! Test-only expressions for the host `MetaM` service fixtures.

    This module deliberately imports no `LeanRsHostShims.*` modules.
    The L1 fixture dylib stays independent from the L2 shim package at
    link time; L2 tests import shim modules into sessions separately. -/

namespace LeanRsFixture.Meta

open Lean

@[reducible] def ReducibleNatAlias : Type := Nat

@[irreducible] def IrreducibleNatAlias : Type := Nat

@[export lean_rs_fixture_meta_expr_nat]
def exprNat (_ : Unit) : Expr :=
  .const ``Nat []

@[export lean_rs_fixture_meta_expr_bool]
def exprBool (_ : Unit) : Expr :=
  .const ``Bool []

@[export lean_rs_fixture_meta_expr_reducible_nat_alias]
def exprReducibleNatAlias (_ : Unit) : Expr :=
  .const ``ReducibleNatAlias []

@[export lean_rs_fixture_meta_expr_irreducible_nat_alias]
def exprIrreducibleNatAlias (_ : Unit) : Expr :=
  .const ``IrreducibleNatAlias []

private def mkSuccExpr : Nat → Expr
  | 0 => .const ``Nat.zero []
  | n + 1 => .app (.const ``Nat.succ []) (mkSuccExpr n)

@[export lean_rs_fixture_meta_expr_large_nat_left]
def exprLargeNatLeft (_ : Unit) : Expr :=
  mkSuccExpr 4096

@[export lean_rs_fixture_meta_expr_large_nat_right]
def exprLargeNatRight (_ : Unit) : Expr :=
  mkSuccExpr 4096

end LeanRsFixture.Meta
