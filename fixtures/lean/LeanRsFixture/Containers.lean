/-! ABI category: container types and ctor structures. Arrays cross as
    `lean_object*` with `lean_array_*` accessors; `Option` and `Except` use the
    inductive tag encoding; the `Pair` structure exercises ctor field layout. -/

namespace LeanRsFixture.Containers

@[export lean_rs_fixture_array_string_identity]
def arrayStringIdentity (xs : Array String) : Array String := xs

/-- Pushes one element. Returns a fresh `Array String`; reuse-vs-copy is decided
    by the runtime based on the unique-reference check. -/
@[export lean_rs_fixture_array_string_push]
def arrayStringPush (xs : Array String) (x : String) : Array String := xs.push x

@[export lean_rs_fixture_option_nat_identity]
def optionNatIdentity (x : Option Nat) : Option Nat := x

@[export lean_rs_fixture_option_nat_some]
def optionNatSome (n : Nat) : Option Nat := some n

@[export lean_rs_fixture_option_nat_none]
def optionNatNone : Option Nat := none

@[export lean_rs_fixture_except_string_nat_ok]
def exceptStringNatOk (n : Nat) : Except String Nat := .ok n

@[export lean_rs_fixture_except_string_nat_err]
def exceptStringNatErr (s : String) : Except String Nat := .error s

/-- Mixed-field structure: one boxed `Nat` field and one boxed `String` field.
    Exercises ctor field offset computation on the Rust side. -/
structure Pair where
  first  : Nat
  second : String

@[export lean_rs_fixture_pair_make]
def pairMake (n : Nat) (s : String) : Pair := { first := n, second := s }

/-- Mixed-field structure: one string field and one array-of-strings field.
    Exercises ctor field layout when one slot is itself a nested container. -/
structure Bundle where
  name  : String
  items : Array String

@[export lean_rs_fixture_bundle_make]
def bundleMake (name : String) (items : Array String) : Bundle :=
  { name, items }

@[export lean_rs_fixture_bundle_identity]
def bundleIdentity (b : Bundle) : Bundle := b

end LeanRsFixture.Containers
