//! Metadata requests carry actual session facts without changing the base handshake.
use lean_rs_worker_protocol::protocol::{MAX_FRAME_BYTES, Message, Request, Response, read_frame, write_frame};
use lean_rs_worker_protocol::types::{LeanWorkerHostRuntimeExpectation, LeanWorkerHostRuntimeMetadata};
use std::collections::BTreeMap;

fn actual() -> LeanWorkerHostRuntimeMetadata {
    LeanWorkerHostRuntimeMetadata {
        compiled_lean_version: "4.35.0".into(),
        compiled_header_sha256: "b".repeat(64),
        runtime_lean_version: Some("4.35.0".into()),
        checked_host_bindings: BTreeMap::from([("verify_declaration_batch".into(), "a".repeat(64))]),
    }
}
#[test]
fn opt_in_request_and_actual_response_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    for message in [
        Message::Request(Request::HostRuntimeMetadata),
        Message::Response(Response::HostArtifactEpochChanged),
        Message::Response(Response::HostRuntimeMetadata { metadata: actual() }),
    ] {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, message.clone(), MAX_FRAME_BYTES)?;
        assert_eq!(read_frame(&mut bytes.as_slice(), MAX_FRAME_BYTES)?.message, message);
    }
    Ok(())
}

#[test]
fn expected_abi_is_a_subset_and_extra_compatible_bindings_are_allowed() {
    let mut observed = actual();
    let required = LeanWorkerHostRuntimeExpectation {
        compiled_header_sha256: Some("b".repeat(64)),
        runtime_lean_version: Some("4.35.0".into()),
        checked_host_bindings: observed.checked_host_bindings.clone(),
    };
    observed
        .checked_host_bindings
        .insert("new_optional_binding".into(), "c".repeat(64));
    assert!(required.validate(&observed).is_ok());
    observed
        .checked_host_bindings
        .insert("verify_declaration_batch".into(), "d".repeat(64));
    assert!(required.validate(&observed).is_err());
}
#[test]
fn missing_runtime_version_or_wrong_compiled_header_is_not_accepted() {
    let required = LeanWorkerHostRuntimeExpectation {
        compiled_header_sha256: Some("b".repeat(64)),
        runtime_lean_version: Some("4.35.0".into()),
        ..Default::default()
    };
    let mut observed = actual();
    observed.runtime_lean_version = None;
    assert!(required.validate(&observed).is_err());
    observed = actual();
    observed.compiled_header_sha256 = "c".repeat(64);
    assert!(required.validate(&observed).is_err());
}
