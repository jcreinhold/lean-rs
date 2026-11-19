import LeanRsInterop

/-! The smallest real Lean capability used by ordinary worker qualification. -/
namespace LeanRsInteropConsumer.Callback

@[export lean_rs_interop_consumer_worker_metadata]
def workerMetadata (_requestJson : String) : IO String :=
  pure "{\"commands\":[{\"name\":\"version\",\"version\":\"fixture-1\"},{\"name\":\"scan\",\"version\":\"fixture-2\"}],\"capabilities\":[{\"name\":\"rows.json\",\"version\":\"fixture-1\"},{\"name\":\"diagnostics\",\"version\":\"fixture-1\"}],\"lean_version\":\"fixture-lean-4\",\"extra\":{\"fixture\":true}}"

end LeanRsInteropConsumer.Callback
