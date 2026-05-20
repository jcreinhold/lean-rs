/-! String callback ABI helper.

    Rust supplies an opaque handle and a trampoline value. Lean treats both as
    `USize` tokens and calls the linked C helper with a borrowed Lean
    `String`. The Rust trampoline copies the string before user code runs.
 -/

namespace LeanRsInterop.Callback.String

@[extern "lean_rs_interop_string_callback_call"]
opaque call (handle : USize) (trampoline : USize) (payload : @& String) : BaseIO UInt8

partial def loopCore (handle trampoline : USize) (payloads : Array String) (idx : Nat) : IO UInt8 := do
  if h : idx < payloads.size then
    let status ← call handle trampoline payloads[idx]
    if status == 0 then
      loopCore handle trampoline payloads (idx + 1)
    else
      pure status
  else
    pure 0

def loop (handle trampoline : USize) (payloads : Array String) : IO UInt8 :=
  loopCore handle trampoline payloads 0

end LeanRsInterop.Callback.String
