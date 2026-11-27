//! Metadata tests launch the sealed stream worker when its qualification embeds one.
use lean_rs_worker_parent::LeanWorkerCapabilityBuilder;

pub(super) fn builder() -> LeanWorkerCapabilityBuilder {
    super::sealed_stream_builder::selected().unwrap_or_else(super::builder)
}
