//! Reusable build-script helpers for downstream embedders.
//!
//! Inside this workspace `lean-rs-sys`'s `build.rs` is the single source of
//! `cargo:rustc-link-*` directives — `lean-toolchain` does not call into the
//! helper from its own build script. The helper exists for **downstream
//! embedders** whose own `build.rs` would otherwise duplicate the link-policy
//! probe, the directive set, and the runtime rpath logic.
//!
//! Usage in a downstream `build.rs`:
//!
//! ```ignore
//! fn main() {
//!     lean_toolchain::emit_lean_link_directives();
//! }
//! ```
//!
//! That one call covers link-time (the `cargo:rustc-link-search` /
//! `link-lib` directives) and load-time (the rpath into the Lean toolchain's
//! `lib/lean` directory) so a consumer binary runs without
//! `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH` set.

use std::env;
use std::sync::OnceLock;

use crate::discover::{DiscoverOptions, ToolchainInfo, discover_toolchain};

/// Set once on the first call to make repeat calls (e.g. multiple
/// `build.rs` invocations within one process) cheap and idempotent.
static EMITTED: OnceLock<()> = OnceLock::new();

/// Emit Lean link-search, link-lib, and runtime rpath directives plus
/// matching rerun triggers from a downstream `build.rs`.
///
/// On the first call this:
///
/// 1. Runs [`discover_toolchain`] with [`DiscoverOptions::default()`].
/// 2. On success, prints `cargo:rustc-link-search=native=<prefix>/lib/lean`
///    and `<prefix>/lib`, plus `cargo:rustc-link-lib=dylib=leanshared`.
/// 3. On macOS or Linux build targets, also prints
///    `cargo:rustc-link-arg=-Wl,-rpath,<prefix>/lib/lean` so the resulting
///    binary loads `libleanshared` without `DYLD_FALLBACK_LIBRARY_PATH` /
///    `LD_LIBRARY_PATH`. Other targets get no rpath directive.
/// 4. On discovery failure, prints one `cargo:warning=` line with the
///    formatted diagnostic and returns; the caller's build then fails at
///    link time with a more specific error from rustc.
/// 5. Emits `cargo:rerun-if-changed=<header>` and the env-var triggers
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

    // Runtime rpath so the consumer binary finds `libleanshared` without
    // `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH`. Gated on the build
    // target (not the host) via `CARGO_CFG_TARGET_OS`; `-Wl,-rpath` is a
    // GNU-ld / lld / Apple-ld flag and is not meaningful on Windows.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "macos" | "linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_lean.display());
    }

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
