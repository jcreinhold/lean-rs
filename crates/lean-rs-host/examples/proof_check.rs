//! Check the application-attested theorem in its originating session, then revalidate and summarize its evidence.

#![allow(clippy::print_stdout, clippy::panic)]

use lean_rs::LeanResult;
use lean_rs::host_bridge::TrustedHostKernelRequest;
use lean_rs_host::{EvidenceStatus, LeanKernelOutcome, LeanSession};

/// Check the application-attested theorem in its originating session, then revalidate and summarize its evidence.
///
/// # Errors
/// Returns session dispatch errors to the application.
///
/// # Panics
/// Panics if checked evidence fails revalidation.
pub fn check<'lean>(
    session: &mut LeanSession<'lean, '_>,
    request: &TrustedHostKernelRequest<'lean, '_>,
) -> LeanResult<()> {
    let outcome = session.kernel_check(request, None, None)?;
    // The outcome is a four-tag enum. Only `Checked` carries a
    // `LeanEvidence` handle for re-validation; the other three carry
    // structured diagnostics.
    let evidence = match outcome {
        LeanKernelOutcome::Checked(evidence) => evidence,
        LeanKernelOutcome::Rejected(failure) => {
            eprintln!("kernel rejected the proof: {failure}");
            return Ok(());
        }
        LeanKernelOutcome::Unavailable(failure) => {
            eprintln!("kernel could not check (resource-bound): {failure}");
            return Ok(());
        }
        LeanKernelOutcome::Unsupported(failure) => {
            eprintln!("source not supported by `kernel_check`: {failure}");
            return Ok(());
        }
    };

    // Re-validate the captured evidence against the (unchanged)
    // session environment. `check_evidence` runs the kernel fresh—
    // useful when you cached evidence and want to confirm it still
    // holds.
    let status = session.check_evidence(&evidence, None)?;
    assert_eq!(status, EvidenceStatus::Checked, "evidence must re-validate");
    println!("check_evidence: {status:?}");

    // Project the evidence into a bounded `ProofSummary` for
    // logging. The strings are display text, not semantic keys.
    let summary = session.summarize_evidence(&evidence, None)?;
    println!(
        "summary: name={} kind={} type={}",
        summary.declaration_name(),
        summary.kind(),
        summary.type_signature(),
    );

    Ok(())
}
