//! Elaborate an application-attested term and handle each response from Meta.inferType.

#![allow(clippy::print_stdout, clippy::panic)]

use lean_rs::LeanResult;
use lean_rs::host_bridge::TrustedHostTerm;
use lean_rs_host::LeanSession;
use lean_rs_host::meta::{LeanMetaOptions, LeanMetaResponse, infer_type};

/// Elaborate an application-attested term and handle each response from Meta.inferType.
///
/// # Errors
/// Returns session dispatch errors to the application.
pub fn infer<'lean>(session: &mut LeanSession<'lean, '_>, term: &TrustedHostTerm<'lean, '_>) -> LeanResult<()> {
    let expr = match session.elaborate(term, None)? {
        Ok(expr) => expr,
        Err(failure) => {
            eprintln!("term elaboration failed: {failure:?}");
            return Ok(());
        }
    };
    // Dispatch `infer_type` with default options (the published
    // heartbeat ceiling, the published diagnostic byte limit). The
    // typed return is `LeanMetaResponse<LeanExpr<'lean>>`.
    let meta_opts = LeanMetaOptions::new();
    let response = session.run_meta(&infer_type(), expr, &meta_opts, None)?;

    match response {
        LeanMetaResponse::Ok(_inferred_expr) => {
            // `LeanExpr` is opaque on purpose—the host stack never
            // pretty-prints proof terms. The `Ok` variant proves the
            // service ran cleanly; downstream callers usually feed
            // the inferred handle into further `MetaM` work or back
            // into `summarize_evidence` via a kernel-check round.
            println!("status=Ok service=infer_type");
        }
        LeanMetaResponse::Failed(failure) => {
            println!("status=Failed: {failure}");
        }
        LeanMetaResponse::TimeoutOrHeartbeat(failure) => {
            println!("status=TimeoutOrHeartbeat: {failure}");
        }
        LeanMetaResponse::Unsupported(failure) => {
            // Reached when the loaded capability lacks the requested
            // `MetaM` shim, or when the Lean side returned
            // `unsupported` for the request shape.
            println!("status=Unsupported: {failure}");
        }
    }

    Ok(())
}
