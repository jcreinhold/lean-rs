import LeanRsInterop

/-! Downstream-style consumer of `lean-rs-interop-shims`.

    This fixture intentionally does not import `LeanRsHostShims`. It proves
    that a capability package can use the generic callback helper without
    depending on theorem-prover host policy.
 -/

namespace LeanRsInteropConsumer.Callback

@[export lean_rs_interop_consumer_add]
def add (a b : UInt64) : UInt64 :=
  a + b

@[export lean_rs_interop_consumer_callback_loop]
def callbackLoop (handle trampoline : USize) (total : UInt64) : IO UInt8 :=
  LeanRsInterop.Callback.Tick.loop handle trampoline total

@[export lean_rs_interop_consumer_string_callback_loop]
def stringCallbackLoop (handle trampoline : USize) (payloads : Array String) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline payloads

def jsonlRows : Array String :=
  #[
    "{\"kind\":\"module\",\"name\":\"LeanRsInteropConsumer\"}",
    "{\"kind\":\"declaration\",\"name\":\"lean_rs_interop_consumer_add\"}",
    "{\"kind\":\"done\",\"count\":2}"
  ]

@[export lean_rs_interop_consumer_jsonl_stream]
def jsonlStream (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline jsonlRows

end LeanRsInteropConsumer.Callback
