/-! Test-only interop callback spike.

    This module is not part of the stable host session contract. It proves the
    reusable ABI shape used by later callback work: Lean receives an opaque
    Rust handle and a Rust trampoline function pointer as `USize`, then calls a
    tiny C helper linked into the shim dylib. The C helper casts the pointer and
    invokes it on the same thread. -/

namespace LeanRsFixture.Interop

@[extern "lean_rs_interop_callback_call"]
opaque callbackCall (handle : USize) (trampoline : USize) (current total : UInt64) : BaseIO UInt8

partial def callbackLoopCore (handle trampoline : USize) (current total : UInt64) : IO UInt8 := do
  if current < total then
    let status ← callbackCall handle trampoline current total
    if status == 0 then
      callbackLoopCore handle trampoline (current + 1) total
    else
      pure status
  else
    pure 0

@[export lean_rs_interop_test_callback_loop]
def callbackLoop (handle trampoline : USize) (total : UInt64) : IO UInt8 :=
  callbackLoopCore handle trampoline 0 total

end LeanRsFixture.Interop
