//! Manifest expectations for the packaged host shim. The runtime bridge owns native bindings.
use lean_rs::module::LeanExportSignature;

pub(crate) fn host_shim_export_signatures() -> Vec<LeanExportSignature> {
    lean_toolchain::host_shim_abi::compiled_host_binding_signatures()
        .into_values()
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions should fail loudly and include precise fixture context"
)]
mod tests {
    use super::host_shim_export_signatures;
    use lean_toolchain::{CapabilityManifest, LeanExportSignature};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn shim_source_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("shims")
            .join("lean-rs-host-shims")
            .join("LeanRsHostShims")
    }

    fn exported_symbols_from_sources() -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut stack = vec![shim_source_dir()];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).expect("read shim source dir") {
                let entry = entry.expect("read shim source entry");
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(std::ffi::OsStr::to_str) != Some("lean") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read shim source");
                for line in text.lines() {
                    let Some(start) = line.find("@[export lean_rs_host_") else {
                        continue;
                    };
                    let after_export = &line[start + "@[export ".len()..];
                    let Some(end) = after_export.find(']') else {
                        continue;
                    };
                    out.insert(after_export[..end].to_owned());
                }
            }
        }
        out
    }

    #[test]
    fn generated_signature_table_matches_packaged_shim_exports() {
        let generated = host_shim_export_signatures()
            .into_iter()
            .map(|signature| signature.symbol().to_owned())
            .collect::<BTreeSet<_>>();
        let source = exported_symbols_from_sources();

        assert_eq!(generated, source);
    }

    #[test]
    fn bundled_shim_manifest_contains_generated_signatures() {
        let path = if let Some(selection) = option_env!("LEAN_RS_TEST_ORDINARY_HOST_SHIMS") {
            #[derive(serde::Deserialize)]
            struct Selection {
                manifest: PathBuf,
            }
            serde_json::from_str::<Selection>(selection)
                .expect("fixture shim selection")
                .manifest
        } else {
            let selection = crate::host::lake::ShimOutputSelection::resolve()
                .expect("supported exact Lean sysroot selects shim outputs");
            crate::host::lake::LakeProject::shim_capability(host_shim_export_signatures(), &selection)
                .expect("bundled shim capability builds")
                .manifest_path()
                .to_owned()
        };
        let manifest = CapabilityManifest::read(&path).expect("manifest parses");
        let generated = signatures_by_symbol(host_shim_export_signatures());
        let manifest_exports = signatures_by_symbol(manifest.exports);

        assert_eq!(manifest_exports, generated);
    }

    fn signatures_by_symbol(signatures: Vec<LeanExportSignature>) -> BTreeMap<String, LeanExportSignature> {
        signatures
            .into_iter()
            .map(|signature| (signature.symbol().to_owned(), signature))
            .collect()
    }
}
