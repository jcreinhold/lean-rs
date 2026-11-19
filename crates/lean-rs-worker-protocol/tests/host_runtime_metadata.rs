//! Metadata requests carry actual session facts without changing the base handshake.
use lean_rs_worker_protocol::protocol::{MAX_FRAME_BYTES, Message, Request, Response, read_frame, write_frame};
use lean_rs_worker_protocol::types::{LeanWorkerHostRuntimeExpectation, LeanWorkerHostRuntimeMetadata};
use std::collections::BTreeMap;

fn actual() -> LeanWorkerHostRuntimeMetadata {
    LeanWorkerHostRuntimeMetadata {
        effective_import_roots: None,
        artifact_read_epochs: None,
        compiled_lean_version: "4.35.0".into(),
        compiled_header_sha256: "b".repeat(64),
        runtime_lean_version: Some("4.35.0".into()),
        checked_host_bindings: BTreeMap::from([("verify_declaration_batch".into(), "a".repeat(64))]),
    }
}
#[test]
fn opt_in_request_and_actual_response_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let mut metadata = actual();
    metadata.artifact_read_epochs = Some(vec![lean_rs_worker_protocol::types::LeanWorkerArtifactReadEpoch {
        project_root: "/canonical/project".into(),
        epoch: "opaque-owned-writer-17".into(),
    }]);
    metadata.effective_import_roots = Some(["/project", "/shim", "/shim", "/builtin"].map(str::to_owned).to_vec());
    for message in [
        Message::Request(Request::HostRuntimeMetadata),
        Message::Request(Request::ReleaseHostRead),
        Message::Response(Response::HostReadReleased),
        Message::Response(Response::HostArtifactEpochChanged),
        Message::Response(Response::HostRuntimeMetadata { metadata }),
    ] {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, message.clone(), MAX_FRAME_BYTES)?;
        assert_eq!(read_frame(&mut bytes.as_slice(), MAX_FRAME_BYTES)?.message, message);
    }
    Ok(())
}
#[test]
fn optional_import_roots_interoperate_with_legacy_metadata() -> Result<(), Box<dyn std::error::Error>> {
    // The previous wire shape. Deriving against these fields also exercises an
    // old reader's treatment of the additive field emitted by a new worker.
    #[derive(serde::Deserialize, serde::Serialize)]
    struct LegacyMetadata {
        compiled_lean_version: String,
        compiled_header_sha256: String,
        runtime_lean_version: Option<String>,
        checked_host_bindings: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_read_epochs: Option<Vec<lean_rs_worker_protocol::types::LeanWorkerArtifactReadEpoch>>,
    }
    let mut modern = actual();
    let legacy_wire = serde_json::to_value(&modern)?;
    assert!(legacy_wire.get("effective_import_roots").is_none());
    assert_eq!(
        serde_json::from_value::<LeanWorkerHostRuntimeMetadata>(legacy_wire.clone())?,
        modern
    );
    modern.effective_import_roots = Some(vec!["/shim".into(), "/shim".into()]);
    let old: LegacyMetadata = serde_json::from_value(serde_json::to_value(modern)?)?;
    assert_eq!(serde_json::to_value(old)?, legacy_wire);
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
