//! The existing fixture commands each named test may call.
use lean_rs_worker_protocol::worker_exports::{metadata_signature, streaming_command_signature};
use lean_toolchain::LeanExportSignature;

#[derive(Clone, Copy)]
pub(crate) enum Exports {
    Streams(&'static [&'static str]),
    Metadata,
}

impl Exports {
    pub(crate) fn signatures(self) -> Vec<LeanExportSignature> {
        match self {
            Self::Streams(suffixes) => suffixes
                .iter()
                .map(|suffix| {
                    streaming_command_signature(format!("lean_rs_interop_consumer_worker_data_stream{suffix}"))
                })
                .collect(),
            Self::Metadata => vec![metadata_signature("lean_rs_interop_consumer_worker_metadata")],
        }
    }
}

pub(crate) const CASES: &[(&[&str], Exports)] = &[
    (
        &[
            "builder_opens_worker_capability_and_validates_metadata",
            "metadata_mismatch_is_reported_by_bootstrap_check",
            "metadata_validation_failure_is_typed",
            "metadata_mismatch_is_typed",
        ],
        Exports::Metadata,
    ),
    (
        &[
            "successful_stream_delivers_rows_with_per_stream_sequences",
            "typed_streaming_command_decodes_rows_and_summary",
            "typed_streaming_command_row_decode_error_names_stream_and_sequence",
            "typed_streaming_command_summary_decode_error_is_typed",
            "raw_row_streaming_remains_available",
            "row_sink_cancellation_cycles_child_and_invalidates_session",
            "diagnostic_sink_panic_is_typed",
        ],
        Exports::Streams(&[""]),
    ),
    (&["malformed_row_json_is_typed"], Exports::Streams(&["_malformed_json"])),
    (
        &["child_fatal_exit_is_reported_to_parent"],
        Exports::Streams(&["_panic"]),
    ),
    (
        &["row_before_child_panic_is_delivered_before_terminal_failure"],
        Exports::Streams(&["_row_then_panic"]),
    ),
    (
        &["missing_stream_or_payload_is_typed"],
        Exports::Streams(&["_missing_stream", "_missing_payload"]),
    ),
    (&["nonzero_export_status_is_typed"], Exports::Streams(&["_status"])),
    (
        &["callback_status_error_is_typed"],
        Exports::Streams(&["_wrong_callback"]),
    ),
    (
        &["large_stream_records_live_forwarding_throughput_and_rss"],
        Exports::Streams(&["_many"]),
    ),
    (
        &["helper_chunked_stream_delivers_rows_diagnostics_progress_and_metadata"],
        Exports::Streams(&["_chunked"]),
    ),
    (
        &["helper_chunked_stream_reports_bounded_chunk_errors"],
        Exports::Streams(&["_chunk_error"]),
    ),
    (
        &["helper_completion_order_stream_uses_same_typed_surface"],
        Exports::Streams(&["_chunked_completion"]),
    ),
    (
        &["typed_streaming_command_decodes_large_payload_from_performance_path"],
        Exports::Streams(&["_large_payload"]),
    ),
];
