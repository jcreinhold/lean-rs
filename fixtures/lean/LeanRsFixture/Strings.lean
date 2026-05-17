/-! ABI category: heap-allocated byte sequences. `String` and `ByteArray` cross
    as `lean_object*`; Rust must call `lean_string_*` / `lean_sarray_*` helpers
    to read or construct them. -/

namespace LeanRsFixture.Strings

@[export lean_rs_fixture_string_identity]
def stringIdentity (s : String) : String := s

/-- Returns the UTF-8 character length as a `Nat` — exercises round-tripping a
    `String` argument and constructing a fresh `Nat` result. -/
@[export lean_rs_fixture_string_length]
def stringLength (s : String) : Nat := s.length

@[export lean_rs_fixture_bytearray_identity]
def bytearrayIdentity (b : ByteArray) : ByteArray := b

@[export lean_rs_fixture_bytearray_size]
def bytearraySize (b : ByteArray) : Nat := b.size

end LeanRsFixture.Strings
