//! Runtime memory guardrail entry points.
//!
//! These symbols are exported by `libleanshared` but are not part of
//! `lean.h`'s object ABI. They configure Lean's best-effort memory checks;
//! they do not reclaim retained runtime state.

unsafe extern "C" {
    /// Set Lean's process-global maximum memory in bytes.
    ///
    /// # Safety
    ///
    /// This changes process-global Lean runtime state. Callers must ensure
    /// they are configuring an isolated worker/profiling process or otherwise
    /// understand that every later Lean runtime check in the process observes
    /// the new limit.
    pub fn lean_internal_set_max_memory(max: usize) -> crate::lean_obj_res;
}
