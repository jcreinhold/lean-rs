//! Bounding helpers for tracing-field values.
//!
//! One case recurs in the span instrumentation: filesystem paths
//! emitted as span fields (`library.path`, `project.root`). For
//! human-readable logs we don't need the full absolute path — the last
//! two parents plus the basename are enough to identify the artefact
//! and short enough to keep one span on one line in a typical terminal.
//!
//! Lean-authored text (capability messages, diagnostic messages) is
//! already bounded at construction time via
//! [`crate::error::bound_message`]; tracing fields that carry it pass
//! the already-bounded string through.

use std::path::Path;

use crate::error::bound_message;

/// Render `path` for a tracing field: keep the basename and up to two
/// parent components. Returns `"<unknown>"` for an empty path.
///
/// Examples:
///
/// - `/Users/me/lake/build/lib/lib.dylib` → `lake/build/lib/lib.dylib`
/// - `/tmp/lib.dylib`                     → `tmp/lib.dylib`
/// - `lib.dylib`                          → `lib.dylib`
///
/// The shortened form is always preferred for `info`/`debug` spans.
/// Full-path emission is left to a `trace`-level event the call site
/// can add when it genuinely needs the absolute path.
pub(crate) fn short_path(path: &Path) -> String {
    let mut tail: Vec<String> = path
        .components()
        .rev()
        .take(3)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if tail.is_empty() {
        return "<unknown>".to_owned();
    }
    tail.reverse();
    let mut joined = tail.join("/");
    // Bound for paranoia: very long basenames (a 4 KiB-name fixture
    // would be pathological but is technically reachable on macOS).
    if joined.len() > crate::LEAN_ERROR_MESSAGE_LIMIT {
        joined = bound_message(joined);
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_path_keeps_three_tail_components() {
        assert_eq!(
            short_path(Path::new("/Users/me/lake/build/lib/lib.dylib")),
            "build/lib/lib.dylib",
        );
    }

    #[test]
    fn short_path_handles_short_input() {
        assert_eq!(short_path(Path::new("lib.dylib")), "lib.dylib");
        assert_eq!(short_path(Path::new("/tmp/lib.dylib")), "tmp/lib.dylib");
    }

    #[test]
    fn short_path_empty_path_falls_back_to_placeholder() {
        assert_eq!(short_path(Path::new("")), "<unknown>");
    }
}
