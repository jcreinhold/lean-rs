import Lean

/-! ABI category: semantic-handle round-trip exports. `Name`, `Level`, `Expr`,
    and `Declaration` cross as opaque `lean_object*`; Rust receives them as
    `LeanName`, `LeanLevel`, `LeanExpr`, `LeanDeclaration` handles that carry
    the value without claiming structural meaning. Construction and inspection
    live here on the Lean side. -/

namespace LeanRsFixture.Handles

open Lean

-- Names ---------------------------------------------------------------

/-- Returns the anonymous root name. `Unit` argument so Lake emits a function
    symbol rather than a persistent global slot. -/
@[export lean_rs_fixture_name_anonymous]
def nameAnonymous (_ : Unit) : Name := .anonymous

@[export lean_rs_fixture_name_mk_str]
def nameMkStr (parent : Name) (s : String) : Name := .str parent s

/-- `UInt64` argument because Lake emits an unboxed scalar for it, matching
    the `LeanAbi` pure-scalar path; the Lean body widens to `Nat`. -/
@[export lean_rs_fixture_name_mk_num]
def nameMkNum (parent : Name) (n : UInt64) : Name := .num parent n.toNat

@[export lean_rs_fixture_name_to_string]
def nameToString (n : Name) : String := n.toString

@[export lean_rs_fixture_name_beq]
def nameBeq (a b : Name) : Bool := a == b

-- Levels --------------------------------------------------------------

@[export lean_rs_fixture_level_zero]
def levelZero (_ : Unit) : Level := .zero

@[export lean_rs_fixture_level_succ]
def levelSucc (u : Level) : Level := .succ u

@[export lean_rs_fixture_level_max]
def levelMax (u v : Level) : Level := .max u v

@[export lean_rs_fixture_level_to_string]
def levelToString (u : Level) : String := toString u

@[export lean_rs_fixture_level_beq]
def levelBeq (u v : Level) : Bool := u == v

-- Expressions ---------------------------------------------------------

/-- A trivial constant expression — `Nat` at the empty universe list. Enough
    to prove the handle round-trips through a Lean-authored constructor
    without exposing `Expr.const` to Rust. -/
@[export lean_rs_fixture_expr_const_nat]
def exprConstNat (_ : Unit) : Expr := .const ``Nat []

@[export lean_rs_fixture_expr_bvar]
def exprBVar (n : UInt64) : Expr := .bvar n.toNat

@[export lean_rs_fixture_expr_app]
def exprApp (f a : Expr) : Expr := .app f a

@[export lean_rs_fixture_expr_to_string]
def exprToString (e : Expr) : String := toString e

@[export lean_rs_fixture_expr_beq]
def exprBeq (a b : Expr) : Bool := a == b

-- Declarations --------------------------------------------------------

/-- Build a minimal axiom declaration of type `Nat` with the given name and
    no universe parameters. Avoids the full mutual / inductive machinery
    while still exercising `Declaration` as a round-trip handle. -/
@[export lean_rs_fixture_declaration_demo_axiom]
def declarationDemoAxiom (name : Name) : Declaration :=
  .axiomDecl {
    name        := name
    levelParams := []
    type        := .const ``Nat []
    isUnsafe    := false
  }

/-- Render a `Declaration` as `"<ctor> <name>"`. Pattern-matching all
    constructors keeps the test diagnostic stable across declaration
    kinds; the prefix tells the test which shape it inspected without
    pretending to summarise the full structure. -/
@[export lean_rs_fixture_declaration_name_to_string]
def declarationNameToString (d : Declaration) : String :=
  match d with
  | .axiomDecl v     => s!"axiom {v.name}"
  | .defnDecl v      => s!"def {v.name}"
  | .thmDecl v       => s!"theorem {v.name}"
  | .opaqueDecl v    => s!"opaque {v.name}"
  | .quotDecl        => "quot"
  | .mutualDefnDecl _ => "mutual"
  | .inductDecl _ _ _ _ => "inductive"

end LeanRsFixture.Handles
