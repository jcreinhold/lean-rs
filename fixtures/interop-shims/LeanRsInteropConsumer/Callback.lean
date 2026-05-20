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

def workerDataRows : Array String :=
  #[
    "{\"stream\":\"rows\",\"payload\":{\"kind\":\"request\",\"ordinal\":0}}",
    "{\"stream\":\"diagnostics\",\"payload\":{\"severity\":\"info\",\"message\":\"started\"}}",
    "{\"stream\":\"rows\",\"payload\":{\"kind\":\"done\",\"ordinal\":1}}"
  ]

@[export lean_rs_interop_consumer_worker_data_stream]
def workerDataStream (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline workerDataRows

@[export lean_rs_interop_consumer_worker_data_stream_malformed_json]
def workerDataStreamMalformedJson (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline #["{not-json"]

@[export lean_rs_interop_consumer_worker_data_stream_missing_stream]
def workerDataStreamMissingStream (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline #["{\"payload\":{\"kind\":\"missing-stream\"}}"]

@[export lean_rs_interop_consumer_worker_data_stream_missing_payload]
def workerDataStreamMissingPayload (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline #["{\"stream\":\"rows\"}"]

@[export lean_rs_interop_consumer_worker_data_stream_status]
def workerDataStreamStatus (_requestJson : String) (_handle _trampoline : USize) : IO UInt8 :=
  pure 7

@[export lean_rs_interop_consumer_worker_data_stream_wrong_callback]
def workerDataStreamWrongCallback (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.Tick.loop handle trampoline 1

@[export lean_rs_interop_consumer_worker_data_stream_panic]
def workerDataStreamPanic (_requestJson : String) (_handle _trampoline : USize) : IO UInt8 :=
  panic! "lean-rs worker stream panic"

end LeanRsInteropConsumer.Callback
