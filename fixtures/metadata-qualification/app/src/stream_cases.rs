//! The existing fixture commands each named test may call.
pub(crate) const CASES: &[(&[&str], &[&str])] = &[
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
        &[""],
    ),
    (&["malformed_row_json_is_typed"], &["_malformed_json"]),
    (
        &["missing_stream_or_payload_is_typed"],
        &["_missing_stream", "_missing_payload"],
    ),
    (&["nonzero_export_status_is_typed"], &["_status"]),
    (&["callback_status_error_is_typed"], &["_wrong_callback"]),
    (&["large_stream_records_live_forwarding_throughput_and_rss"], &["_many"]),
    (
        &["helper_chunked_stream_delivers_rows_diagnostics_progress_and_metadata"],
        &["_chunked"],
    ),
    (
        &["helper_chunked_stream_reports_bounded_chunk_errors"],
        &["_chunk_error"],
    ),
    (
        &["helper_completion_order_stream_uses_same_typed_surface"],
        &["_chunked_completion"],
    ),
    (
        &["typed_streaming_command_decodes_large_payload_from_performance_path"],
        &["_large_payload"],
    ),
];

pub(crate) fn symbol(suffix: &str) -> String {
    format!("lean_rs_interop_consumer_worker_data_stream{suffix}")
}
