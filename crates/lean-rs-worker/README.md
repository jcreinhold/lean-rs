# lean-rs-worker

Internal worker-process boundary for `lean-rs` host workloads.

This crate is not yet the public supervisor API. Prompt 56 uses it to prove a
private framed protocol and child runner; prompt 57 will add the caller-facing
`LeanWorker` surface.
