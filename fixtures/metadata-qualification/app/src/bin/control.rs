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
    if mode == "collect-closed" {
        let selected = PathBuf::from(
            option_env!("LEAN_RS_METADATA_DEPLOYMENT_RECORD").ok_or("missing selected deployment record")?,
        );
        let expected = selected
            .parent()
            .ok_or("selected deployment has no state directory")?
            .join("owners");
        let owners = PathBuf::from(std::env::var_os("LEAN_RS_WORKER_OWNERS_DIRECTORY").ok_or("missing owners")?);
        if owners != expected || owners != owners.canonicalize()? {
            return Err("collector owners differ from the reviewed disposable deployment".into());
        }
        lean_toolchain::native_guardian::collect_closed_owner_records(&owners, Duration::from_secs(2))?;
        let mut entries = std::fs::read_dir(&owners)?;
        let entry = entries.next().ok_or("missing admission lock")??;
        if entry.file_name() != ".admission.lock" || entries.next().is_some() {
            return Err("closed collector left an unexpected registry entry".into());
        }
        println!("closed owner records collected");
        return Ok(());
    }
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
    let owners = PathBuf::from(std::env::var_os("LEAN_RS_WORKER_OWNERS_DIRECTORY").ok_or("missing owners")?);
    let state = owners.parent().ok_or("owners have no state directory")?;
    let qualification = state.parent().ok_or("state has no qualification directory")?;
    let cargo_target = qualification.parent().ok_or("state is not below the Cargo target")?;
    if owners.file_name().and_then(|name| name.to_str()) != Some("owners")
        || state.file_name().and_then(|name| name.to_str()) != Some("ordinary")
        || qualification.file_name().and_then(|name| name.to_str()) != Some("metadata-qualification")
    {
        return Err("owners path is outside the ordinary metadata state".into());
    }
    let socket_root = cargo_target.join("ws");
    let child = LeanWorkerChild::path(worker)
        .shared_writer_executable(Record::helper())
        .native_guardian(Record::helper())
        .lifetime_directory(owners)
        .lifetime_socket_directory(socket_root)
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
        .expect_metadata(EXPORT, json!({"caller":"private-metadata-fixture"}), selected.clone());
    match (mode.as_str(), builder.open()) {
        ("positive", Ok(cap)) => {
            assert_eq!(cap.validated_metadata(), Some(&expected));
            let mut effective: Vec<_> = d.import_roots.iter().map(|p| p.to_str().unwrap().to_owned()).collect();
            effective.push(d.sysroot.join("lib/lean").to_str().unwrap().to_owned());
            let runtime = cap.host_runtime_metadata().ok_or("missing runtime metadata")?;
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
        ("reversed" | "wrong-image" | "wrong-initializer" | "wrong-ordered-directories", Err(e)) => {
            println!("refusal {e:?}")
        }
        (_, Err(e)) => return Err(format!("unexpected result: {e:?}").into()),
        (_, Ok(_)) => return Err("unexpected success".into()),
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../../../crates/lean-rs-host/tests/support/transitive_assertions.rs"]
mod transitive_assertions;

#[test]
fn import_finds_transitive_lake_package_oleans() -> Result<(), Box<dyn std::error::Error>> {
    use lean_rs::{LeanRuntime, module::LoadedHostArtifacts};
    use lean_rs_host::host::LeanPreparedHostArtifacts;
    use lean_rs_host::{
        LeanCapabilities, LeanHost, LeanImportProfileMode, LeanImportProfilerOptions, LeanSessionImportProfile,
    };
    use metadata_qualification::{Held, audit};
    use std::sync::Arc;

    const WORKER_CASE: &str = "shims_only_host_handle_imports_transitive_lake_package_oleans";
    let held = Arc::new(Held::acquire(Record::selected())?);
    let r = &held.record;
    assert!(r.broken_import && !r.typed_session && !r.stream && !r.verification);
    assert_eq!(r.host_builder_case.as_deref(), Some(WORKER_CASE));
    assert_eq!(
        r.project.file_name().and_then(|name| name.to_str()),
        Some("lean-transitive")
    );
    held.verify()?;
    held.check_interop_visibility(false)?;

    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, &r.project)?;
    let prepared = LeanPreparedHostArtifacts::from_snapshot(
        r.shim_manifest.clone(),
        r.shim_json.clone(),
        r.shim_roots.clone(),
        r.shim_paths.clone(),
    )?;
    // SAFETY: Held verified the exact sealed loader inputs and retains the
    // consumer, path dependency and both shim locks across the direct call.
    let loaded = unsafe {
        LoadedHostArtifacts::load(
            runtime,
            r.shim_manifest.clone(),
            r.shim_json.as_bytes().to_vec(),
            r.shim_roots.clone(),
            r.shim_paths.clone(),
            Arc::clone(&held),
        )?
    };
    held.check_interop_visibility(true)?;
    let caps = LeanCapabilities::from_prepared(&host, &prepared, loaded)?;
    assert_eq!(caps.artifact_roots(), r.roots);
    let paths = caps.import_search_paths();
    transitive_assertions::check(&caps, |module| {
        let names = vec![module.to_owned()];
        let options = LeanImportProfileMode::FullSession(LeanSessionImportProfile::default())
            .native_options(&LeanImportProfilerOptions::default());
        r.check_order(&paths, &names, &options)
            .expect("exact sealed import graph");
        // SAFETY: check_order admits only the two reviewed modules, in this
        // exact order of held artifact roots and with default import options.
        unsafe {
            caps.shim_capability()
                .import_scope_with_options(paths.clone(), names, options)
        }
    });
    audit("direct-host-transitive-passed");
    Ok(())
}
