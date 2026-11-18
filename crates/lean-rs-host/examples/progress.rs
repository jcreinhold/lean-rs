//! Observe progress and cancellation in a session importing LeanRsFixture.Handles.

#![allow(clippy::print_stdout, clippy::panic)]

use lean_rs::{LeanDiagnosticCode, LeanResult};
use lean_rs_host::{LeanCancellationToken, LeanProgressEvent, LeanProgressSink, LeanSession};

struct PrintProgress;

impl LeanProgressSink for PrintProgress {
    fn report(&self, event: LeanProgressEvent) {
        let total = event.total.map_or_else(|| "-".to_owned(), |n| n.to_string());
        println!(
            "progress phase={} current={} total={} elapsed_us={}",
            event.phase,
            event.current,
            total,
            event.elapsed.as_micros()
        );
    }
}

struct CancelOnFirst<'a> {
    token: &'a LeanCancellationToken,
}

impl LeanProgressSink for CancelOnFirst<'_> {
    fn report(&self, event: LeanProgressEvent) {
        println!(
            "cancel_progress phase={} current={} total={}",
            event.phase,
            event.current,
            event.total.map_or_else(|| "-".to_owned(), |n| n.to_string())
        );
        if event.current >= 1 {
            self.token.cancel();
        }
    }
}

/// Observe progress and cancellation in a session importing LeanRsFixture.Handles.
///
/// # Errors
/// Returns session dispatch errors to the application.
///
/// # Panics
/// Panics if the pre-cancelled query unexpectedly succeeds.
pub fn report_and_cancel(session: &mut LeanSession<'_, '_>) -> LeanResult<()> {
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];

    let progress = PrintProgress;
    let declarations = session.query_declarations_bulk(&names, None, Some(&progress))?;
    println!("queried_declarations={}", declarations.len());

    let token = LeanCancellationToken::new();
    let cancel_sink = CancelOnFirst { token: &token };
    match session.declaration_kind_bulk(&names, Some(&token), Some(&cancel_sink)) {
        Err(err) if err.code() == LeanDiagnosticCode::Cancelled => {
            println!("cancelled_code={}", err.code());
        }
        Err(err) => return Err(err),
        Ok(kinds) => panic!("expected cancellation, got {kinds:?}"),
    }

    Ok(())
}
