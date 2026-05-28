//! Bounded module-query projection capability on [`crate::LeanSession`].
//!
//! [`crate::LeanSession::process_module_query`] takes a full Lean source
//! file plus a [`ModuleQuery`]. Lean parses the header, elaborates the
//! body, performs the requested cursor/reference/diagnostic projection,
//! and returns only that bounded result. Raw whole-file `InfoTree`
//! projections do not cross the FFI boundary.

mod query;

pub use self::query::{
    DeclarationTargetInfo, DeclarationTargetResult, DeclarationVerificationFacts, DeclarationVerificationOutcome,
    DeclarationVerificationRequest, DeclarationVerificationStatus, DeclarationVerificationTarget, GoalAtResult,
    LocalInfo, ModuleQuery, ModuleQueryBatchCachedOutcome, ModuleQueryBatchEnvelope, ModuleQueryBatchItem,
    ModuleQueryBatchOutcome, ModuleQueryBatchResult, ModuleQueryCacheFacts, ModuleQueryCachePolicy,
    ModuleQueryCacheStatus, ModuleQueryOutcome, ModuleQueryOutputBudgets, ModuleQueryResult, ModuleQuerySelector,
    ModuleQueryTimings, ModuleSnapshotCacheClearResult, ModuleSourceSpan, NameRefNode, ProofAttemptEnvelope,
    ProofAttemptOutcome, ProofAttemptRequest, ProofAttemptRow, ProofAttemptStatus, ProofCandidate, ProofEditTarget,
    ProofStateInfo, ProofStateResult, ReferencesResult, RenderedInfo, SorryPolicy, SurroundingDeclarationResult,
    TypeAtResult,
};
