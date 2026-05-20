# lean-rs-worker

Worker-process boundary for `lean-rs` host workloads.

`LeanWorker` supervises one `lean-rs-worker-child` process. It hides process
spawning, stdio pipes, protocol framing, child-exit parsing, and cleanup behind
a small lifecycle API. The in-process theorem-prover stack remains in
`lean-rs-host`; this crate owns the production process boundary around it.
