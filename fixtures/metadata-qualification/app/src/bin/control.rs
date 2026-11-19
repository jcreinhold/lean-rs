use lean_rs::LeanBuiltCapability;
use lean_rs_worker_parent::{
    LeanWorkerCapabilityBuilder, LeanWorkerCapabilityMetadata, LeanWorkerChild, LeanWorkerError,
};
use metadata_qualification::{EXPORT, MODULE, Record};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).ok_or("mode")?;
    let d = Record::selected();
    let mut prepared = d.descriptor();
    if mode == "reversed" {
        prepared.shim_search_paths.reverse();
    }
    // This application supplies this exact DTO to the SDK launcher for every open.
    // Compare the candidate with the independent build record before it can spawn a child.
    if prepared != d.descriptor() {
        if mode == "reversed" {
            println!("rejected reordered prepared roots before worker construction");
            return Ok(());
        }
        return Err("unknown prepared deployment".into());
    }
    let worker = std::env::current_exe()?.with_file_name("metadata-worker");
    let child = LeanWorkerChild::path(worker)
        .shared_writer_executable(Record::helper())
        .native_guardian(Record::helper())
        .lifetime_directory(PathBuf::from(
            std::env::var_os("LEAN_RS_WORKER_OWNERS_DIRECTORY").unwrap(),
        ))
        .prepared_host_artifacts(prepared)
        .shared_artifact_reads(true);
    let target = LeanBuiltCapability::manifest_path(&d.consumer_manifest);
    let expected: LeanWorkerCapabilityMetadata = serde_json::from_value(json!({
        "commands":[{"name":"version","version":"fixture-1"},{"name":"scan","version":"fixture-2"}],
        "capabilities":[{"name":"rows.json","version":"fixture-1"},{"name":"diagnostics","version":"fixture-1"}],
        "lean_version":"fixture-lean-4","extra":{"fixture":true}}))?;
    let mut selected = expected.clone();
    if mode == "mismatch" {
        selected.commands[0].version = "deliberately-wrong".into()
    }
    let builder = LeanWorkerCapabilityBuilder::from_built_capability(&target, [MODULE])?
        .worker_child(child)
        .request_timeout(Duration::from_secs(100))
        .startup_timeout(Duration::from_secs(40))
        .require_host_runtime(Default::default())
        .expect_metadata(
            EXPORT,
            json!({"caller":"private-metadata-fixture"}),
            selected.clone(),
        );
    match (mode.as_str(), builder.open()) {
        ("positive", Ok(cap)) => {
            assert_eq!(cap.validated_metadata(), Some(&expected));
            let mut effective: Vec<_> = d
                .import_roots
                .iter()
                .map(|p| p.to_str().unwrap().to_owned())
                .collect();
            effective.push(d.sysroot.join("lib/lean").to_str().unwrap().to_owned());
            let runtime = cap
                .host_runtime_metadata()
                .ok_or("missing runtime metadata")?;
            assert_eq!(runtime.effective_import_roots.as_ref(), Some(&effective));
            let mut domains: Vec<_> = runtime
                .artifact_read_epochs
                .as_ref()
                .ok_or("missing epochs")?
                .iter()
                .map(|e| PathBuf::from(&e.project_root))
                .collect();
            domains.sort();
            assert_eq!(domains, d.roots);
            println!(
                "complete native metadata {}\nordered roots {}\nepochs {}",
                serde_json::to_string(&expected)?,
                serde_json::to_string(&effective)?,
                serde_json::to_string(&runtime.artifact_read_epochs)?
            );
            println!("shutdown {:?}", cap.shutdown()?.outcome);
        }
        (
            "mismatch",
            Err(LeanWorkerError::CapabilityMetadataMismatch {
                export,
                expected: wanted,
                actual,
            }),
        ) => {
            assert_eq!(export, EXPORT);
            assert_eq!(*wanted, selected);
            assert_eq!(*actual, expected);
            println!(
                "CapabilityMetadataMismatch after exact export; actual {}",
                serde_json::to_string(&actual)?
            );
        }
        (
            "reversed" | "wrong-image" | "wrong-initializer" | "wrong-ordered-directories",
            Err(e),
        ) => println!("refusal {e:?}"),
        (_, Err(e)) => return Err(format!("unexpected result: {e:?}").into()),
        (_, Ok(_)) => return Err("unexpected success".into()),
    }
    Ok(())
}
