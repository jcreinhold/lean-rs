//! The fixed interop consumer exports each named interop test may load.
use lean_rs_worker_protocol::worker_exports::{doctor_signature, metadata_signature};
use lean_toolchain::LeanExportSignature;

/// The file name of the capability manifest `prepare` writes for interop runs.
/// The worker opens only a manifest with this name.
pub(crate) const MANIFEST_NAME: &str = "worker-interop-capability.json";

#[derive(Clone, Copy)]
pub(crate) enum Exports {
    Metadata,
    Doctor,
    MalformedMetadataAndDoctor,
    NoExports,
}

impl Exports {
    pub(crate) fn signatures(self) -> Vec<LeanExportSignature> {
        match self {
            Self::Metadata => vec![metadata_signature("lean_rs_interop_consumer_worker_metadata")],
            Self::Doctor => vec![doctor_signature("lean_rs_interop_consumer_worker_doctor")],
            Self::MalformedMetadataAndDoctor => vec![
                metadata_signature("lean_rs_interop_consumer_worker_metadata_malformed"),
                doctor_signature("lean_rs_interop_consumer_worker_doctor_malformed"),
            ],
            Self::NoExports => Vec::new(),
        }
    }
}

pub(crate) const CASES: &[(&[&str], Exports)] = &[
    (&["session_import_profile_reaches_child_import"], Exports::NoExports),
    (&["runtime_and_capability_metadata_are_separate"], Exports::Metadata),
    (&["doctor_reports_pass_warning_and_error_diagnostics"], Exports::Doctor),
    (
        &["malformed_metadata_and_doctor_json_are_typed"],
        Exports::MalformedMetadataAndDoctor,
    ),
    (
        &["missing_metadata_and_doctor_exports_are_typed_worker_errors"],
        Exports::NoExports,
    ),
];
