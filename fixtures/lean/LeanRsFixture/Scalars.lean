/-! ABI category: unboxed fixed-width scalars and boxed primitive scalars.
    Each `_identity` export round-trips one ABI type. Arithmetic exports cover
    the case where Rust must marshal arguments and inspect the returned value. -/

namespace LeanRsFixture.Scalars

/-! ## Fixed-width unsigned integers (unboxed at the C boundary). -/

@[export lean_rs_fixture_u8_identity]
def u8Identity (x : UInt8) : UInt8 := x

@[export lean_rs_fixture_u16_identity]
def u16Identity (x : UInt16) : UInt16 := x

@[export lean_rs_fixture_u32_identity]
def u32Identity (x : UInt32) : UInt32 := x

@[export lean_rs_fixture_u64_identity]
def u64Identity (x : UInt64) : UInt64 := x

@[export lean_rs_fixture_usize_identity]
def usizeIdentity (x : USize) : USize := x

/-- Exercises argument marshaling for two unboxed `UInt32` parameters. -/
@[export lean_rs_fixture_u32_add]
def u32Add (a b : UInt32) : UInt32 := a + b

/-- Exercises wider-than-pointer return on 32-bit targets. -/
@[export lean_rs_fixture_u64_mul]
def u64Mul (a b : UInt64) : UInt64 := a * b

/-! ## `Nat` and `Int` — boxed via `lean_object*`, but small values fit a tagged scalar. -/

@[export lean_rs_fixture_nat_identity]
def natIdentity (n : Nat) : Nat := n

/-- Returns `n+1`; small inputs stay in the tagged-scalar encoding, large ones force a bignum. -/
@[export lean_rs_fixture_nat_succ]
def natSucc (n : Nat) : Nat := n + 1

@[export lean_rs_fixture_int_identity]
def intIdentity (n : Int) : Int := n

@[export lean_rs_fixture_int_neg]
def intNeg (n : Int) : Int := -n

/-! ## `Bool`, `Unit`, `Char`, `Float`. -/

/-- `Bool` is a two-constructor inductive; both constructors are boxed as `lean_box(0)`/`lean_box(1)`. -/
@[export lean_rs_fixture_bool_not]
def boolNot (b : Bool) : Bool := !b

/-- `Unit` is `lean_box(0)`. -/
@[export lean_rs_fixture_unit_id]
def unitId (u : Unit) : Unit := u

/-- `Char` is an unboxed `uint32_t` carrying a Unicode scalar value. -/
@[export lean_rs_fixture_char_identity]
def charIdentity (c : Char) : Char := c

/-- `Float` is an unboxed C `double`. -/
@[export lean_rs_fixture_float_identity]
def floatIdentity (x : Float) : Float := x

@[export lean_rs_fixture_float_add]
def floatAdd (a b : Float) : Float := a + b

end LeanRsFixture.Scalars
