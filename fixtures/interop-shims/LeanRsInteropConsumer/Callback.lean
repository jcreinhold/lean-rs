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
    "{\"diagnostic\":{\"code\":\"lean_rs.worker.fixture.started\",\"message\":\"started\"}}",
    "{\"stream\":\"rows\",\"payload\":{\"kind\":\"done\",\"ordinal\":1}}",
    "{\"diagnostic\":{\"code\":\"lean_rs.worker.fixture.finished\",\"message\":\"finished\"}}",
    "{\"metadata\":{\"fixture\":\"worker_data_stream\",\"ok\":true}}"
  ]

@[export lean_rs_interop_consumer_worker_data_stream]
def workerDataStream (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline workerDataRows

def manyWorkerDataRows (count : Nat) : Array String := Id.run do
  let mut rows := #[]
  for i in [0:count] do
    rows := rows.push ("{\"stream\":\"rows\",\"payload\":{\"i\":" ++ toString i ++ "}}")
  rows

@[export lean_rs_interop_consumer_worker_data_stream_many]
def workerDataStreamMany (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline (manyWorkerDataRows 512)

def largeWorkerPayload : String :=
  String.ofList (List.replicate 8192 'x')

@[export lean_rs_interop_consumer_worker_data_stream_large_payload]
def workerDataStreamLargePayload (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline
    #[ "{\"stream\":\"rows\",\"payload\":{\"kind\":\"large\",\"blob\":\"" ++ largeWorkerPayload ++ "\"}}" ]

@[export lean_rs_interop_consumer_worker_data_stream_slow_after_row]
def workerDataStreamSlowAfterRow (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← LeanRsInterop.Callback.String.loop handle trampoline
    #["{\"stream\":\"rows\",\"payload\":{\"kind\":\"before-timeout\"}}"]
  if status == 0 then
    IO.sleep 200
    pure 0
  else
    pure status

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

@[export lean_rs_interop_consumer_worker_data_stream_row_then_panic]
def workerDataStreamRowThenPanic (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← LeanRsInterop.Callback.String.loop handle trampoline
    #["{\"stream\":\"rows\",\"payload\":{\"kind\":\"before-panic\"}}"]
  if status == 0 then
    panic! "lean-rs worker stream panic after row"
  else
    pure status

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

@[export lean_rs_interop_consumer_worker_json_command]
def workerJsonCommand (_requestJson : String) : IO String :=
  pure "{\"accepted\":true,\"kind\":\"fixture\"}"

@[export lean_rs_interop_consumer_worker_json_command_malformed]
def workerJsonCommandMalformed (_requestJson : String) : IO String :=
  pure "{not-json"

@[export lean_rs_interop_consumer_worker_shape_metadata]
def workerShapeMetadata (_requestJson : String) : IO String :=
  pure "{\"commands\":[{\"name\":\"version\",\"version\":\"shape-1\"},{\"name\":\"doctor\",\"version\":\"shape-1\"},{\"name\":\"extract\",\"version\":\"shape-1\"},{\"name\":\"features\",\"version\":\"shape-1\"},{\"name\":\"index\",\"version\":\"shape-1\"},{\"name\":\"probe\",\"version\":\"shape-1\"}],\"capabilities\":[{\"name\":\"rows.json.raw\",\"version\":\"shape-1\"},{\"name\":\"diagnostics\",\"version\":\"shape-1\"},{\"name\":\"terminal-metadata\",\"version\":\"shape-1\"}],\"lean_version\":\"fixture-lean-4\",\"extra\":{\"fixture\":\"lean-dup-shaped\",\"schema\":\"generic\"}}"

@[export lean_rs_interop_consumer_worker_shape_doctor]
def workerShapeDoctor (_requestJson : String) : IO String :=
  pure "{\"diagnostics\":[{\"severity\":\"pass\",\"code\":\"shape.lake\",\"message\":\"Lake target is available\",\"details\":{\"target\":\"LeanRsInteropConsumer\"}},{\"severity\":\"warning\",\"code\":\"shape.cache\",\"message\":\"cache policy is downstream-owned\",\"details\":{\"owner\":\"downstream\"}},{\"severity\":\"pass\",\"code\":\"shape.streams\",\"message\":\"streaming callbacks are available\",\"details\":{\"streams\":[\"declarations\",\"features\",\"probes\"]}}],\"metadata\":{\"fixture\":\"lean-dup-shaped\",\"ok\":true}}"

@[export lean_rs_interop_consumer_worker_shape_version]
def workerShapeVersion (_requestJson : String) : IO String :=
  pure "{\"worker\":\"lean-rs-worker-fixture\",\"protocol\":\"shape-1\",\"commands\":[\"version\",\"doctor\",\"extract\",\"features\",\"index\",\"probe\"],\"capabilities\":[\"rows\",\"diagnostics\",\"metadata\"]}"

def workerShapeMetadataFor (command : String) (rows : Nat) : String :=
  "{\"metadata\":{\"fixture\":\"lean-dup-shaped\",\"command\":\"" ++ command ++
    "\",\"ok\":true,\"rows\":" ++ toString rows ++ "}}"

def workerShapeExtractRows : Array String :=
  #[
    "{\"diagnostic\":{\"code\":\"shape.extract.started\",\"message\":\"extract started\"}}",
    "{\"stream\":\"declarations\",\"payload\":{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.alpha\",\"ordinal\":0}}",
    "{\"stream\":\"declarations\",\"payload\":{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beta\",\"ordinal\":1}}",
    "{\"diagnostic\":{\"code\":\"shape.extract.finished\",\"message\":\"extract finished\"}}",
    workerShapeMetadataFor "extract" 2
  ]

def workerShapeFeatureRows : Array String :=
  #[
    "{\"diagnostic\":{\"code\":\"shape.features.started\",\"message\":\"features started\"}}",
    "{\"stream\":\"features\",\"payload\":{\"kind\":\"feature\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.alpha\",\"feature\":\"namespace\",\"score\":1,\"ordinal\":0}}",
    "{\"stream\":\"features\",\"payload\":{\"kind\":\"feature\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beta\",\"feature\":\"type-shape\",\"score\":2,\"ordinal\":1}}",
    "{\"diagnostic\":{\"code\":\"shape.features.finished\",\"message\":\"features finished\"}}",
    workerShapeMetadataFor "features" 2
  ]

def workerShapeIndexRows : Array String :=
  #[
    "{\"diagnostic\":{\"code\":\"shape.index.imported\",\"message\":\"import-once fixture ready\"}}",
    "{\"stream\":\"declarations\",\"payload\":{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.alpha\",\"ordinal\":0}}",
    "{\"stream\":\"features\",\"payload\":{\"kind\":\"feature\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.alpha\",\"feature\":\"namespace\",\"score\":1,\"ordinal\":0}}",
    "{\"stream\":\"declarations\",\"payload\":{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beta\",\"ordinal\":1}}",
    "{\"stream\":\"features\",\"payload\":{\"kind\":\"feature\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beta\",\"feature\":\"type-shape\",\"score\":2,\"ordinal\":1}}",
    "{\"diagnostic\":{\"code\":\"shape.index.finished\",\"message\":\"index finished\"}}",
    workerShapeMetadataFor "index" 4
  ]

def workerShapeProbeRows : Array String :=
  #[
    "{\"diagnostic\":{\"code\":\"shape.probe.started\",\"message\":\"probe started\"}}",
    "{\"stream\":\"probes\",\"payload\":{\"kind\":\"probe\",\"left\":\"Fixture.alpha\",\"right\":\"Fixture.beta\",\"relation\":\"related\",\"ordinal\":0}}",
    "{\"diagnostic\":{\"code\":\"shape.probe.finished\",\"message\":\"probe finished\"}}",
    workerShapeMetadataFor "probe" 1
  ]

@[export lean_rs_interop_consumer_worker_shape_extract]
def workerShapeExtract (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline workerShapeExtractRows

@[export lean_rs_interop_consumer_worker_shape_features]
def workerShapeFeatures (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline workerShapeFeatureRows

@[export lean_rs_interop_consumer_worker_shape_index]
def workerShapeIndex (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline workerShapeIndexRows

@[export lean_rs_interop_consumer_worker_shape_probe]
def workerShapeProbe (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline workerShapeProbeRows

@[export lean_rs_interop_consumer_worker_shape_timeout_after_row]
def workerShapeTimeoutAfterRow (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← LeanRsInterop.Callback.String.loop handle trampoline
    #["{\"stream\":\"declarations\",\"payload\":{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.timeout\",\"ordinal\":0}}"]
  if status == 0 then
    IO.sleep 200
    pure 0
  else
    pure status

@[export lean_rs_interop_consumer_worker_shape_panic_after_row]
def workerShapePanicAfterRow (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← LeanRsInterop.Callback.String.loop handle trampoline
    #["{\"stream\":\"declarations\",\"payload\":{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beforePanic\",\"ordinal\":0}}"]
  if status == 0 then
    panic! "lean-rs worker shape panic after row"
  else
    pure status

end LeanRsInteropConsumer.Callback
