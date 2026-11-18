//! Inspect declaration names and kinds in an application-admitted session.

#![allow(clippy::print_stdout, clippy::panic)]

use lean_rs::LeanResult;
use lean_rs_host::LeanSession;

/// Inspect declaration names and kinds in an application-admitted session.
///
/// # Errors
/// Returns session dispatch errors to the application.
pub fn query(session: &mut LeanSession<'_, '_>) -> LeanResult<()> {
    // The environment carries the full Lean prelude plus the
    // fixture's own declarations. Many thousands of entries even for
    // a small project—print only the count, then ask about names
    // the caller already knows.
    let names = session.list_declarations(None)?;
    println!("total_declarations={}", names.len());

    // Contrast a definition and a theorem by name.
    // `declaration_kind` returns the Lean-rendered tag—
    // `"definition"`, `"theorem"`, `"axiom"`, etc.—
    // `declaration_name` round-trips a name through Lean's pretty
    // printer (diagnostic only, not a semantic key).
    for name in ["Nat.add", "Nat.add_zero"] {
        let kind = session.declaration_kind(name, None)?;
        let rendered = session.declaration_name(name, None)?;
        println!("{name}: kind={kind} rendered={rendered}");
    }

    Ok(())
}
