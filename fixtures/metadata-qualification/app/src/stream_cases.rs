//! The existing fixture commands each named test may call.
use lean_rs_worker_protocol::worker_exports::{
    doctor_signature, json_command_signature, metadata_signature, streaming_command_signature,
};
use lean_toolchain::LeanExportSignature;

#[derive(Clone, Copy)]
pub(crate) enum Exports {
    Streams(&'static [&'static str]),
    Metadata,
    Doctor,
    MalformedMetadataAndDoctor,
    NoExports,
    MalformedJson,
    StreamAndJson(&'static str),
    Shape(&'static [&'static str], &'static [&'static str]),
}

impl Exports {
    pub(crate) fn signatures(self) -> Vec<(LeanExportSignature, bool)> {
        let stream = |suffix: &str| {
            (
                streaming_command_signature(format!("lean_rs_interop_consumer_worker_data_stream{suffix}")),
                true,
            )
        };
        match self {
            Self::Streams(suffixes) => suffixes.iter().map(|suffix| stream(suffix)).collect(),
            Self::Metadata => vec![(metadata_signature("lean_rs_interop_consumer_worker_metadata"), false)],
            Self::Doctor => vec![(doctor_signature("lean_rs_interop_consumer_worker_doctor"), false)],
            Self::MalformedMetadataAndDoctor => vec![
                (
                    metadata_signature("lean_rs_interop_consumer_worker_metadata_malformed"),
                    false,
                ),
                (
                    doctor_signature("lean_rs_interop_consumer_worker_doctor_malformed"),
                    false,
                ),
            ],
            Self::NoExports => Vec::new(),
            Self::MalformedJson => vec![(
                json_command_signature("lean_rs_interop_consumer_worker_json_command_malformed"),
                false,
            )],
            Self::StreamAndJson(suffix) => vec![
                stream(suffix),
                (
                    json_command_signature("lean_rs_interop_consumer_worker_json_command"),
                    false,
                ),
            ],
            Self::Shape(unary, streams) => unary
                .iter()
                .map(|suffix| {
                    (
                        json_command_signature(format!("lean_rs_interop_consumer_worker_shape_{suffix}")),
                        false,
                    )
                })
                .chain(streams.iter().map(|suffix| {
                    (
                        streaming_command_signature(format!("lean_rs_interop_consumer_worker_shape_{suffix}")),
                        true,
                    )
                }))
                .collect(),
        }
    }
}

pub(crate) const CASES: &[(&[&str], Exports)] = &[
    (
        &["metadata_doctor_and_version_use_generic_capability_shapes"],
        Exports::Shape(&["metadata", "doctor", "version"], &[]),
    ),
    (
        &["streaming_commands_use_builder_and_typed_facade"],
        Exports::Shape(&[], &["extract", "features", "index", "probe"]),
    ),
    (
        &["timeout_cancellation_and_fatal_exit_have_distinct_worker_outcomes"],
        Exports::Shape(&[], &["timeout_after_row", "extract", "panic_after_row"]),
    ),
    (
        &["worker_cycle_leaves_shape_fixture_usable"],
        Exports::Shape(&["version"], &[]),
    ),
    (&["typed_json_command_decode_error_is_typed"], Exports::MalformedJson),
    (
        &["child_fatal_exit_invalidates_lease_and_next_acquire_replaces_worker"],
        Exports::StreamAndJson("_row_then_panic"),
    ),
    (
        &[
            "cancellation_invalidates_current_lease_but_future_lease_can_continue",
            "timeout_invalidates_current_lease_but_future_lease_can_continue",
        ],
        Exports::StreamAndJson("_slow_after_row"),
    ),
    (
        &[
            "builder_opens_worker_capability_and_validates_metadata",
            "metadata_mismatch_is_reported_by_bootstrap_check",
            "metadata_validation_failure_is_typed",
            "metadata_mismatch_is_typed",
            "runtime_and_capability_metadata_are_separate",
        ],
        Exports::Metadata,
    ),
    (&["doctor_reports_pass_warning_and_error_diagnostics"], Exports::Doctor),
    (
        &["malformed_metadata_and_doctor_json_are_typed"],
        Exports::MalformedMetadataAndDoctor,
    ),
    (
        &["missing_metadata_and_doctor_exports_are_typed_worker_errors"],
        Exports::NoExports,
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
        &[
            "large_stream_records_live_forwarding_throughput_and_rss",
            "pool_and_lease_snapshots_record_stream_throughput",
            "slow_sink_applies_bounded_backpressure_without_losing_rows",
            "cancellation_during_backpressure_invalidates_lease",
        ],
        Exports::Streams(&["_many"]),
    ),
    (
        &["fatal_exit_after_buffered_rows_records_failure_without_commit"],
        Exports::Streams(&["_many_then_panic"]),
    ),
    (
        &["request_timeout_cycles_child_and_invalidates_session"],
        Exports::Streams(&["_slow_after_row"]),
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
