import LeanRsInterop.Callback.String
import Lean.Data.Json

/-! Worker streaming helpers for downstream capability packages.

    These helpers own the callback-envelope mechanics used by
    `lean-rs-worker`. Downstream packages still own request parsing, row
    schemas, command names, and semantic work.
 -/

namespace LeanRsInterop.Worker.Stream

@[inline]
def jsonString (value : String) : String :=
  (Lean.Json.str value).compress

@[inline]
def row (stream payloadJson : String) : String :=
  "{\"stream\":" ++ jsonString stream ++ ",\"payload\":" ++ payloadJson ++ "}"

@[inline]
def diagnostic (code message : String) : String :=
  "{\"diagnostic\":{\"code\":" ++ jsonString code ++ ",\"message\":" ++ jsonString message ++ "}}"

@[inline]
def progress (phase : String) (current : Nat) (total : Option Nat := none) : String :=
  let totalField :=
    match total with
    | none => ""
    | some total => ",\"total\":" ++ toString total
  "{\"progress\":{\"phase\":" ++ jsonString phase ++ ",\"current\":" ++ toString current ++ totalField ++ "}}"

@[inline]
def chunkProgress (phase : String) (chunkIndex totalChunks : Nat) : String :=
  progress phase (chunkIndex + 1) (some totalChunks)

@[inline]
def metadata (metadataJson : String) : String :=
  "{\"metadata\":" ++ metadataJson ++ "}"

def emitAll (handle trampoline : USize) (payloads : Array String) : IO UInt8 :=
  LeanRsInterop.Callback.String.loop handle trampoline payloads

def countChunks (chunkSize itemCount : Nat) : Nat :=
  let size := max 1 chunkSize
  (itemCount + size - 1) / size

end LeanRsInterop.Worker.Stream
