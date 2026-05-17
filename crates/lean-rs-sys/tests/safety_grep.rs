//! Two-part hygiene check:
//!
//! 1. Every `unsafe { ... }` block and `unsafe fn` carries a `// SAFETY:` or
//!    `# Safety` comment in the surrounding source. The check is purely
//!    lexical — it scans for the keyword and looks for the documentation
//!    marker in the file. Cheap, no new dev-deps.
//! 2. No `pub fn` exists in `src/` without `unsafe`. The crate's public
//!    surface is intrinsically unsafe; a stray safe `pub fn` would slip
//!    past the workspace `unsafe-code = "deny"` lint without the
//!    discipline this test enforces.

// Tests panic on setup failures (unreadable source files etc.); that is
// the right behaviour for a hygiene test, not a smell.
#![allow(clippy::panic, clippy::unreachable)]

use std::fs;
use std::path::PathBuf;

const SOURCE_FILES: &[&str] = &[
    "src/lib.rs",
    "src/consts.rs",
    "src/types.rs",
    "src/repr.rs",
    "src/refcount.rs",
    "src/object.rs",
    "src/scalar.rs",
    "src/nat_int.rs",
    "src/string.rs",
    "src/array.rs",
    "src/closure.rs",
    "src/io.rs",
    "src/init.rs",
    "src/external.rs",
];

fn read(path: &str) -> String {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match fs::read_to_string(base.join(path)) {
        Ok(text) => text,
        Err(err) => unreachable!("test fixture {path} should be readable: {err}"),
    }
}

#[test]
fn every_unsafe_fn_has_safety_section() {
    let mut missing: Vec<String> = Vec::new();
    for path in SOURCE_FILES {
        let src = read(path);
        // Public unsafe fns must have a `# Safety` doc section; private
        // helpers may skip it as long as their inline body remains
        // self-contained.
        let mut needs_safety: Vec<usize> = Vec::new();
        for (idx, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("pub unsafe fn ") || trimmed.starts_with("pub(crate) unsafe fn ")
            {
                needs_safety.push(idx);
            }
        }
        for &decl_idx in &needs_safety {
            // Walk back through the preceding doc-comment block.
            let mut found = false;
            let mut cursor = decl_idx;
            while cursor > 0 {
                cursor -= 1;
                let line = src.lines().nth(cursor).unwrap_or("").trim_start();
                if line.starts_with("///") || line.starts_with("//!") {
                    if line.contains("# Safety") {
                        found = true;
                        break;
                    }
                } else if !line.starts_with("#[") && !line.is_empty() {
                    // Crossed a real line of code — stop looking.
                    break;
                }
            }
            if !found {
                missing.push(format!(
                    "{path}:{}: pub unsafe fn missing `# Safety` doc section",
                    decl_idx + 1,
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "unsafe-fn discipline violations:\n  - {}",
        missing.join("\n  - ")
    );
}

#[test]
fn every_unsafe_block_has_safety_comment() {
    let mut missing: Vec<String> = Vec::new();
    for path in SOURCE_FILES {
        let src = read(path);
        let lines: Vec<&str> = src.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            // Skip module attributes and doc comments.
            let trimmed = line.trim_start();
            if !is_unsafe_block(trimmed) {
                continue;
            }
            let mut found = false;
            // Look up to 6 lines back for a `// SAFETY:` comment. The
            // canonical layout is the SAFETY comment one or two lines
            // above the `unsafe {` block opener; we widen the window for
            // helper closures and macro expansions.
            let mut cursor = idx;
            for _ in 0..6 {
                if cursor == 0 {
                    break;
                }
                cursor -= 1;
                let l = lines.get(cursor).map_or("", |s| s.trim_start());
                if l.contains("// SAFETY:") {
                    found = true;
                    break;
                }
                if !l.starts_with("//") && !l.is_empty() && !l.starts_with("#[") {
                    // We crossed a non-comment line; the SAFETY annotation
                    // belongs to whichever code precedes the unsafe block,
                    // and we expect it within the immediate window.
                    if l.contains("// SAFETY:") {
                        found = true;
                    }
                    break;
                }
            }
            if !found {
                missing.push(format!(
                    "{path}:{}: `unsafe {{` block missing `// SAFETY:` comment",
                    idx + 1,
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "unsafe-block discipline violations:\n  - {}",
        missing.join("\n  - ")
    );
}

fn is_unsafe_block(trimmed: &str) -> bool {
    // Match `unsafe {`, `unsafe extern`, `unsafe trait`, etc. — but we only
    // care about `unsafe { ... }` block expressions. The simplest robust
    // test is "the keyword `unsafe` is immediately followed by `{`".
    let mut chars = trimmed.chars();
    let mut buf = String::new();
    for c in chars.by_ref() {
        if c.is_whitespace() {
            if buf == "unsafe" {
                let rest: String = trimmed.chars().skip(buf.len()).collect();
                return rest.trim_start().starts_with('{');
            }
            return false;
        }
        if c == '{' {
            return buf == "unsafe";
        }
        buf.push(c);
    }
    false
}

#[test]
fn no_safe_public_functions_in_public_surface() {
    let mut violations: Vec<String> = Vec::new();
    for path in SOURCE_FILES {
        let src = read(path);
        let mut extern_depth: i32 = 0;
        for (idx, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            // Track `unsafe extern "C" { ... }` blocks; `pub fn` declarations
            // inside them are extern declarations and are inherently unsafe
            // to call (the `extern "C"` block carries the contract).
            if trimmed.starts_with("unsafe extern ") || trimmed.starts_with("extern \"C\"") {
                if trimmed.contains('{') {
                    extern_depth += 1;
                }
                continue;
            }
            if extern_depth > 0 {
                if trimmed == "}" || trimmed.starts_with("} ") {
                    extern_depth -= 1;
                }
                continue;
            }
            if trimmed.starts_with("pub fn ") {
                violations.push(format!(
                    "{path}:{}: `pub fn` found outside `extern \"C\"` — public surface must be `pub unsafe fn`",
                    idx + 1,
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "public surface must be intrinsically unsafe:\n  - {}",
        violations.join("\n  - ")
    );
}
