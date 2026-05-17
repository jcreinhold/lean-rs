//! Reusable build-script helpers for downstream embedders.
//!
//! Inside this workspace `lean-rs-sys`'s `build.rs` is the single source of
//! `cargo:rustc-link-*` directives — `lean-toolchain` does not call into the
//! helper from its own build script. The helper exists for **downstream
//! embedders** whose own `build.rs` would otherwise duplicate the link-policy
//! probe and directive set.
//!
//! Usage in a downstream `build.rs`:
//!
//! ```ignore
//! fn main() {
//!     lean_toolchain::emit_lean_link_directives();
//! }
//! ```

use std::sync::OnceLock;

use crate::discover::{DiscoverOptions, ToolchainInfo, discover_toolchain};

/// Set once on the first call to make repeat calls (e.g. multiple
/// `build.rs` invocations within one process) cheap and idempotent.
static EMITTED: OnceLock<()> = OnceLock::new();

/// Emit Lean link-search / link-lib directives and the matching rerun
/// triggers from a downstream `build.rs`.
///
/// On the first call this:
///
/// 1. Runs [`discover_toolchain`] with [`DiscoverOptions::default()`].
/// 2. On success, prints `cargo:rustc-link-search=native=<prefix>/lib/lean`
///    and `<prefix>/lib`, plus `cargo:rustc-link-lib=dylib=leanshared`.
/// 3. On failure, prints one `cargo:warning=` line with the formatted
///    diagnostic and returns; the caller's build then fails at link time
///    with a more specific error from rustc.
/// 4. Emits `cargo:rerun-if-changed=<header>` and the env-var triggers
///    discovery consults.
///
/// Subsequent calls within the same process are no-ops.
pub fn emit_lean_link_directives() {
    if EMITTED.set(()).is_err() {
        return;
    }

    match discover_toolchain(&DiscoverOptions::default()) {
        Ok(info) => emit_for(&info),
        Err(diagnostic) => {
            println!("cargo:warning={diagnostic}");
            emit_rerun_triggers(None);
        }
    }
}

fn emit_for(info: &ToolchainInfo) {
    let lib_lean = info.lib_dir.join("lean");
    println!("cargo:rustc-link-search=native={}", lib_lean.display());
    println!("cargo:rustc-link-search=native={}", info.lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=leanshared");
    emit_rerun_triggers(Some(info));
}

fn emit_rerun_triggers(info: Option<&ToolchainInfo>) {
    if let Some(info) = info {
        println!("cargo:rerun-if-changed={}", info.header_path.display());
    }
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=ELAN_HOME");
    println!("cargo:rerun-if-env-changed=PATH");
}
