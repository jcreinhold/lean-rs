# lean-rs-worker

Worker-process boundary for `lean-rs` host workloads.

`LeanWorker` supervises one `lean-rs-worker-child` process. It hides process
spawning, stdio pipes, protocol framing, child-exit parsing, and cleanup behind
a small lifecycle API. The in-process theorem-prover stack remains in
`lean-rs-host`; this crate owns the production process boundary around it.

`LeanWorkerRestartPolicy` cycles the child process before requests when a
configured request count, import-like request count, RSS ceiling, or idle
duration is reached. Cycling is a process restart. It is the reset boundary for
Lean process-global runtime and import memory; `SessionPool::drain()` remains an
in-process cache operation, not an RSS reset.
