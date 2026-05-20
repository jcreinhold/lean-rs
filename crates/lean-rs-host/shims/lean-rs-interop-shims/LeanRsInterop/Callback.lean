/-! Generic callback ABI helpers for Lean-to-Rust calls.

    Rust supplies an opaque handle and a trampoline value. Lean treats both as
    `USize` tokens and calls the C helper linked by this package. The helper
    invokes the Rust trampoline on the same thread and returns a `UInt8`
    status byte.
 -/

namespace LeanRsInterop.Callback

@[extern "lean_rs_interop_callback_call"]
opaque call (handle : USize) (trampoline : USize) (current total : UInt64) : BaseIO UInt8

partial def loopCore (handle trampoline : USize) (current total : UInt64) : IO UInt8 := do
  if current < total then
    let status ← call handle trampoline current total
    if status == 0 then
      loopCore handle trampoline (current + 1) total
    else
      pure status
  else
    pure 0

def loop (handle trampoline : USize) (total : UInt64) : IO UInt8 :=
  loopCore handle trampoline 0 total

end LeanRsInterop.Callback
