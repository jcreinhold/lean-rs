//! Process-global Lean runtime memory guardrails.
//!
//! This is intentionally separate from runtime initialization and object
//! ownership. The setter does not reclaim memory; it only configures Lean's
//! periodic runtime memory check.

// SAFETY DOC: this module is the narrow safe wrapper around Lean's raw
// process-global memory-limit setter.
#![allow(unsafe_code)]

pub(crate) fn set_memory_limit_bytes_for_guardrail(limit_bytes: usize) {
    // SAFETY: callers route through the hidden sibling-crate bridge and use
    // this only for isolated worker/profiling processes. The call mutates
    // process-global Lean runtime state.
    unsafe {
        let _ = lean_rs_sys::memory::lean_internal_set_max_memory(limit_bytes);
    }
}
