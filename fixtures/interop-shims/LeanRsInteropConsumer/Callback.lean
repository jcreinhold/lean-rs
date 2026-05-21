import LeanRsInterop

/-! Downstream-style consumer of `lean-rs-interop-shims`.

    This fixture intentionally does not import `LeanRsHostShims`. It proves
    that a capability package can use the generic callback helper without
    depending on theorem-prover host policy.
 -/

namespace LeanRsInteropConsumer.Callback

open LeanRsInterop.Worker

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

def workerDataRows (_ : Unit) : Array String :=
  #[
    Stream.row "rows" "{\"kind\":\"request\",\"ordinal\":0}",
    Stream.diagnostic "lean_rs.worker.fixture.started" "started",
    Stream.row "rows" "{\"kind\":\"done\",\"ordinal\":1}",
    Stream.diagnostic "lean_rs.worker.fixture.finished" "finished",
    Stream.metadata "{\"fixture\":\"worker_data_stream\",\"ok\":true}"
  ]

@[export lean_rs_interop_consumer_worker_data_stream]
def workerDataStream (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (workerDataRows ())

def manyWorkerDataRows (count : Nat) : Array String := Id.run do
  let mut rows := #[]
  for i in [0:count] do
    rows := rows.push (Stream.row "rows" ("{\"i\":" ++ toString i ++ "}"))
  rows

@[export lean_rs_interop_consumer_worker_data_stream_many]
def workerDataStreamMany (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (manyWorkerDataRows 512)

def chunkFixturePayload (i : Nat) : String :=
  "{\"kind\":\"chunk\",\"ordinal\":" ++ toString i ++ "}"

def chunkRows (_ : Unit) : Array String :=
  #[
    Stream.row "chunks" (chunkFixturePayload 0),
    Stream.row "chunks" (chunkFixturePayload 1),
    Stream.chunkProgress "fixture.chunk" 0 3,
    Stream.row "chunks" (chunkFixturePayload 2),
    Stream.row "chunks" (chunkFixturePayload 3),
    Stream.chunkProgress "fixture.chunk" 1 3,
    Stream.row "chunks" (chunkFixturePayload 4),
    Stream.row "chunks" (chunkFixturePayload 5),
    Stream.chunkProgress "fixture.chunk" 2 3
  ]

def chunkedStreamRows (_ : Unit) : Array String :=
  Id.run do
    let mut rows := #[Stream.diagnostic "lean_rs.worker.fixture.chunk.started" "chunk stream started"]
    for row in chunkRows () do
      rows := rows.push row
    rows := rows.push (Stream.diagnostic "lean_rs.worker.fixture.chunk.finished" "chunk stream finished")
    rows := rows.push (Stream.metadata "{\"fixture\":\"worker_data_stream_chunks\",\"ok\":true}")
    rows

def chunkErrorRows (_ : Unit) : Array String :=
  #[
    Stream.row "chunks" (chunkFixturePayload 0),
    Stream.row "chunks" (chunkFixturePayload 1),
    Stream.diagnostic "lean_rs.worker.stream.chunk_error" "fixture chunk error"
  ]

@[export lean_rs_interop_consumer_worker_data_stream_chunked]
def workerDataStreamChunked (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (chunkedStreamRows ())

@[export lean_rs_interop_consumer_worker_data_stream_chunked_completion]
def workerDataStreamChunkedCompletion (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  Stream.emitAll handle trampoline (chunkedStreamRows ())

@[export lean_rs_interop_consumer_worker_data_stream_chunk_error]
def workerDataStreamChunkError (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← Stream.emitAll handle trampoline (chunkErrorRows ())
  if status != 0 then
    pure status
  else
    pure 10

def largeWorkerPayload : String :=
  String.ofList (List.replicate 8192 'x')

@[export lean_rs_interop_consumer_worker_data_stream_large_payload]
def workerDataStreamLargePayload (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline
    #[Stream.row "rows" ("{\"kind\":\"large\",\"blob\":" ++ Stream.jsonString largeWorkerPayload ++ "}")]

@[export lean_rs_interop_consumer_worker_data_stream_slow_after_row]
def workerDataStreamSlowAfterRow (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← Stream.emitAll handle trampoline
    #[Stream.row "rows" "{\"kind\":\"before-timeout\"}"]
  if status == 0 then
    IO.sleep 200
    pure 0
  else
    pure status

@[export lean_rs_interop_consumer_worker_data_stream_malformed_json]
def workerDataStreamMalformedJson (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline #["{not-json"]

@[export lean_rs_interop_consumer_worker_data_stream_missing_stream]
def workerDataStreamMissingStream (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline #["{\"payload\":{\"kind\":\"missing-stream\"}}"]

@[export lean_rs_interop_consumer_worker_data_stream_missing_payload]
def workerDataStreamMissingPayload (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline #["{\"stream\":\"rows\"}"]

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
  let status ← Stream.emitAll handle trampoline
    #[Stream.row "rows" "{\"kind\":\"before-panic\"}"]
  if status == 0 then
    panic! "lean-rs worker stream panic after row"
  else
    pure status

@[export lean_rs_interop_consumer_worker_data_stream_many_then_panic]
def workerDataStreamManyThenPanic (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← Stream.emitAll handle trampoline (manyWorkerDataRows 128)
  if status == 0 then
    panic! "lean-rs worker stream panic after many rows"
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
  Stream.metadata
    ("{\"fixture\":\"lean-dup-shaped\",\"command\":" ++ Stream.jsonString command ++
      ",\"ok\":true,\"rows\":" ++ toString rows ++ "}")

def workerShapeExtractRows (_ : Unit) : Array String :=
  #[
    Stream.diagnostic "shape.extract.started" "extract started",
    Stream.row "declarations" "{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.alpha\",\"ordinal\":0}",
    Stream.row "declarations" "{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beta\",\"ordinal\":1}",
    Stream.diagnostic "shape.extract.finished" "extract finished",
    workerShapeMetadataFor "extract" 2
  ]

def workerShapeFeatureRows (_ : Unit) : Array String :=
  #[
    Stream.diagnostic "shape.features.started" "features started",
    Stream.row "features" "{\"kind\":\"feature\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.alpha\",\"feature\":\"namespace\",\"score\":1,\"ordinal\":0}",
    Stream.row "features" "{\"kind\":\"feature\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beta\",\"feature\":\"type-shape\",\"score\":2,\"ordinal\":1}",
    Stream.diagnostic "shape.features.finished" "features finished",
    workerShapeMetadataFor "features" 2
  ]

def workerShapeIndexRows (_ : Unit) : Array String :=
  #[
    Stream.diagnostic "shape.index.imported" "import-once fixture ready",
    Stream.row "declarations" "{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.alpha\",\"ordinal\":0}",
    Stream.row "features" "{\"kind\":\"feature\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.alpha\",\"feature\":\"namespace\",\"score\":1,\"ordinal\":0}",
    Stream.row "declarations" "{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beta\",\"ordinal\":1}",
    Stream.row "features" "{\"kind\":\"feature\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beta\",\"feature\":\"type-shape\",\"score\":2,\"ordinal\":1}",
    Stream.diagnostic "shape.index.finished" "index finished",
    workerShapeMetadataFor "index" 4
  ]

def workerShapeProbeRows (_ : Unit) : Array String :=
  #[
    Stream.diagnostic "shape.probe.started" "probe started",
    Stream.row "probes" "{\"kind\":\"probe\",\"left\":\"Fixture.alpha\",\"right\":\"Fixture.beta\",\"relation\":\"related\",\"ordinal\":0}",
    Stream.diagnostic "shape.probe.finished" "probe finished",
    workerShapeMetadataFor "probe" 1
  ]

@[export lean_rs_interop_consumer_worker_shape_extract]
def workerShapeExtract (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (workerShapeExtractRows ())

@[export lean_rs_interop_consumer_worker_shape_features]
def workerShapeFeatures (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (workerShapeFeatureRows ())

@[export lean_rs_interop_consumer_worker_shape_index]
def workerShapeIndex (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (workerShapeIndexRows ())

@[export lean_rs_interop_consumer_worker_shape_probe]
def workerShapeProbe (_requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (workerShapeProbeRows ())

def mathlibScaleModuleCount : Nat :=
  16

def mathlibScaleModuleName (index : Nat) : String :=
  "Mathlib.Scale.Module" ++ toString index

def mathlibScaleMetadataFor (command : String) (rows modules : Nat) : String :=
  Stream.metadata
    ("{\"fixture\":\"mathlib-scale-shaped\",\"command\":" ++ Stream.jsonString command ++
      ",\"ok\":true,\"rows\":" ++ toString rows ++ ",\"modules\":" ++ toString modules ++ "}")

def mathlibScaleDeclarationPayload (module : String) (ordinal : Nat) : String :=
  let _ := module
  "{\"kind\":\"declaration\",\"module\":\"Mathlib.Scale\",\"name\":\"Mathlib.Scale.decl" ++ toString ordinal ++ "\"" ++
    ",\"ordinal\":" ++ toString ordinal ++ "}"

def mathlibScaleFeaturePayload (module : String) (ordinal : Nat) : String :=
  let _ := module
  "{\"kind\":\"feature\",\"module\":\"Mathlib.Scale\",\"name\":\"Mathlib.Scale.decl" ++ toString ordinal ++ "\"" ++
    ",\"feature\":\"module-shape\",\"score\":" ++ toString ((ordinal % 17) + 1) ++
    ",\"ordinal\":" ++ toString ordinal ++ "}"

def mathlibScaleProbePayload (left right : String) (ordinal : Nat) : String :=
  let _ := left
  let _ := right
  "{\"kind\":\"probe\",\"left\":\"Mathlib.Scale.left\",\"right\":\"Mathlib.Scale.right\"" ++
    ",\"relation\":\"same-import-batch\",\"ordinal\":" ++ toString ordinal ++ "}"

def mathlibScaleIndexRows (_requestJson : String) : Array String := Id.run do
  let limit := 256
  let mut rows := #[Stream.diagnostic "scale.index.started" "mathlib-scale fixture started"]
  let mut emitted := 0
  for moduleIndex in [0:mathlibScaleModuleCount] do
    let module := mathlibScaleModuleName moduleIndex
    if emitted < limit then
      rows := rows.push (Stream.row "declarations" (mathlibScaleDeclarationPayload module emitted))
      emitted := emitted + 1
    if emitted < limit then
      rows := rows.push (Stream.row "features" (mathlibScaleFeaturePayload module emitted))
      emitted := emitted + 1
    if moduleIndex > 0 then
      let previous := mathlibScaleModuleName (moduleIndex - 1)
      if emitted < limit then
        rows := rows.push (Stream.row "probes" (mathlibScaleProbePayload previous module emitted))
        emitted := emitted + 1
    if (moduleIndex + 1) % 4 == 0 then
      rows := rows.push (Stream.chunkProgress "scale.index" (moduleIndex / 4) 4)
  rows := rows.push (Stream.diagnostic "scale.index.finished" "mathlib-scale fixture finished")
  rows := rows.push (mathlibScaleMetadataFor "index" emitted mathlibScaleModuleCount)
  rows

@[export lean_rs_interop_consumer_worker_shape_mathlib_scale_index]
def workerShapeMathlibScaleIndex (requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (mathlibScaleIndexRows requestJson)

@[export lean_rs_interop_consumer_worker_shape_mathlib_scale_extract]
def workerShapeMathlibScaleExtract (requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (mathlibScaleIndexRows requestJson)

@[export lean_rs_interop_consumer_worker_shape_mathlib_scale_features]
def workerShapeMathlibScaleFeatures (requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (mathlibScaleIndexRows requestJson)

@[export lean_rs_interop_consumer_worker_shape_mathlib_scale_probe]
def workerShapeMathlibScaleProbe (requestJson : String) (handle trampoline : USize) : IO UInt8 :=
  Stream.emitAll handle trampoline (mathlibScaleIndexRows requestJson)

@[export lean_rs_interop_consumer_worker_shape_mathlib_scale_timeout_after_row]
def workerShapeMathlibScaleTimeoutAfterRow (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← Stream.emitAll handle trampoline
    #[Stream.row "declarations"
      (mathlibScaleDeclarationPayload "Mathlib.Fixture.Timeout" 0)]
  if status == 0 then
    IO.sleep 200
    pure 0
  else
    pure status

@[export lean_rs_interop_consumer_worker_shape_mathlib_scale_panic_after_row]
def workerShapeMathlibScalePanicAfterRow (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← Stream.emitAll handle trampoline
    #[Stream.row "declarations"
      (mathlibScaleDeclarationPayload "Mathlib.Fixture.Panic" 0)]
  if status == 0 then
    panic! "lean-rs mathlib-scale fixture panic after row"
  else
    pure status

@[export lean_rs_interop_consumer_worker_shape_timeout_after_row]
def workerShapeTimeoutAfterRow (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← Stream.emitAll handle trampoline
    #[Stream.row "declarations"
      "{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.timeout\",\"ordinal\":0}"]
  if status == 0 then
    IO.sleep 200
    pure 0
  else
    pure status

@[export lean_rs_interop_consumer_worker_shape_panic_after_row]
def workerShapePanicAfterRow (_requestJson : String) (handle trampoline : USize) : IO UInt8 := do
  let status ← Stream.emitAll handle trampoline
    #[Stream.row "declarations"
      "{\"kind\":\"declaration\",\"module\":\"Fixture.Basic\",\"name\":\"Fixture.beforePanic\",\"ordinal\":0}"]
  if status == 0 then
    panic! "lean-rs worker shape panic after row"
  else
    pure status

end LeanRsInteropConsumer.Callback
