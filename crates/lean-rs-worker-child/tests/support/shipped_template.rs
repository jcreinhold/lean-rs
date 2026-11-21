use std::path::{Path, PathBuf};

pub(super) fn root() -> PathBuf {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let owner = if option_env!("LEAN_RS_TEST_PRIVATE_SHIPPED_TEMPLATE") == Some("1") {
        sdk.join("target/metadata-qualification/stream-package")
    } else {
        sdk
    };
    owner.join("templates/shipped-lean-crate")
}
