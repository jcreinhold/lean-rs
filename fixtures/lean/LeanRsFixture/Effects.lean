/-! ABI category: `IO` actions. The C representation of `IO α` is a result
    object carrying either a success value or an `IO.Error`. Rust must inspect
    `lean_io_result_is_ok` and either extract the payload or surface the error. -/

namespace LeanRsFixture.Effects

/-- Successful `IO Unit`: Rust sees `lean_io_result_is_ok = true` with a `lean_box(0)` payload. -/
@[export lean_rs_fixture_io_success_unit]
def ioSuccessUnit : IO Unit := pure ()

/-- Successful `IO Nat`: exercises payload extraction from the result object. -/
@[export lean_rs_fixture_io_success_nat]
def ioSuccessNat : IO Nat := pure 7

/-- Outer `IO` succeeds; the inner `Except` is the failure channel. Rust sees
    `lean_io_result_is_ok = true` and must then inspect the returned `Except`. -/
@[export lean_rs_fixture_io_failure]
def ioFailure : IO (Except String Nat) :=
  pure (.error "lean_rs_fixture: deliberate inner failure")

/-- Throws via the `IO` error channel. Rust sees `lean_io_result_is_ok = false`
    and must read the error through `lean_io_result_show_error` or by
    inspecting the returned `IO.Error`. -/
@[export lean_rs_fixture_io_throw]
def ioThrow : IO Nat :=
  throw <| IO.userError "lean_rs_fixture: deliberate IO exception"

end LeanRsFixture.Effects
