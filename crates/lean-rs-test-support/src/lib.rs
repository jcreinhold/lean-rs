//! Test fixtures and helpers shared across the `lean-rs` project's tests.
//!
//! Workspace-internal (`publish = false`). Anything it exposes is for the project's own tests and is explicitly
//! excluded from the public stability surface.

/// Version of the `lean-rs-test-support` crate, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod fixture;

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_constant_matches_package() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
