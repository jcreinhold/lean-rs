/-! Test-only interop callback export.

    This module is not part of the stable host session contract. It keeps the
    prompt-40 test export available from the host shim dylib. The host package
    links the generic interop package's C callback helper source directly so
    this export can be called from a host-only dylib load. -/

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
