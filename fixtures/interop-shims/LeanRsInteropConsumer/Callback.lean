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

@[export lean_rs_interop_consumer_worker_metadata]
def workerMetadata (_requestJson : String) : IO String :=
  pure "{\"commands\":[{\"name\":\"version\",\"version\":\"fixture-1\"},{\"name\":\"scan\",\"version\":\"fixture-2\"}],\"capabilities\":[{\"name\":\"rows.json\",\"version\":\"fixture-1\"},{\"name\":\"diagnostics\",\"version\":\"fixture-1\"}],\"lean_version\":\"fixture-lean-4\",\"extra\":{\"fixture\":true}}"

@[export lean_rs_interop_consumer_worker_metadata_malformed]
def workerMetadataMalformed (_requestJson : String) : IO String :=
  pure "{not-json"

@[export lean_rs_interop_consumer_worker_doctor]
def workerDoctor (_requestJson : String) : IO String :=
  pure "{\"diagnostics\":[{\"severity\":\"pass\",\"code\":\"fixture.ok\",\"message\":\"fixture ready\",\"details\":{\"check\":\"load\"}},{\"severity\":\"warning\",\"code\":\"fixture.warning\",\"message\":\"optional fixture warning\",\"details\":{\"optional\":true}},{\"severity\":\"error\",\"code\":\"fixture.error\",\"message\":\"fixture error example\",\"details\":{\"recoverable\":false}}],\"metadata\":{\"fixture\":\"doctor\"}}"

@[export lean_rs_interop_consumer_worker_doctor_malformed]
def workerDoctorMalformed (_requestJson : String) : IO String :=
  pure "{\"diagnostics\":[{\"severity\":\"bogus\",\"code\":\"fixture.bad\",\"message\":\"bad severity\"}]}"

end LeanRsInteropConsumer.Callback
