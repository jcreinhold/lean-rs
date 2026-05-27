//! In-process [`tracing`] event capture for tests and downstream apps.
//!
//! [`DiagnosticCapture`] installs a per-thread subscriber for its
//! lifetime, buffers up to a bounded number of `lean_rs` events into a
//! [`std::collections::VecDeque`], and exposes them as
//! [`CapturedEvent`] records. On `Drop` the subscriber is uninstalled
//! and the previous default (if any) is restored.
//!
//! This is the always-present test affordance: no cargo feature, no
//! external `tracing-subscriber` install boilerplate. Production
//! downstream applications that want a different sink (`fmt`,
//! `tracing-bunyan`, OpenTelemetry, …) simply install their own
//! subscriber instead of constructing a [`DiagnosticCapture`].

use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::{LookupSpan, Registry};

use crate::error::LeanDiagnosticCode;

/// Soft cap on the number of events a [`DiagnosticCapture`] retains.
///
/// When the buffer is full, the *oldest* event is dropped and
/// [`DiagnosticCapture::overflowed`] increments. The cap exists so a
/// long-running test cannot grow the buffer without bound; tests that
/// expect more than a few hundred events should construct the capture
/// with a larger budget via [`DiagnosticCapture::with_capacity`].
pub const DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY: usize = 256;

/// One captured tracing event.
///
/// Built by the per-thread layer installed by [`DiagnosticCapture`]. The
/// `code` field is populated when an event carries a `code = "..."`
/// field whose value matches one of [`LeanDiagnosticCode::as_str`].
/// `fields` carries other structured fields verbatim, with values
/// rendered via [`fmt::Debug`] (the standard tracing protocol).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedEvent {
    /// The event's target, e.g. `"lean_rs"`. Tracing assigns the
    /// containing module path by default.
    pub target: String,
    /// The event's level as the lowercase string `"error"`, `"warn"`,
    /// `"info"`, `"debug"`, or `"trace"`.
    pub level: &'static str,
    /// The event's `name` (assigned by `tracing` from the source-site
    /// macro), e.g. `"event src/host/session.rs:271"`. Use [`Self::span`]
    /// to identify *which* `#[instrument]`-style span produced the
    /// event.
    pub name: &'static str,
    /// The span name the event was emitted inside, if any. This is
    /// the identifier used by the documented span catalogue
    /// (`lean_rs.host.session.import`, `lean_rs.module.library.open`,
    /// …).
    pub span: Option<String>,
    /// The diagnostic code attached to this event, if it carries a
    /// `code` field that matches a known [`LeanDiagnosticCode`].
    pub code: Option<LeanDiagnosticCode>,
    /// The event's `message` field if present, else an empty string.
    pub message: String,
    /// Other structured fields (`(name, value)` pairs), excluding
    /// `code` and `message`.
    pub fields: Vec<(&'static str, String)>,
}

/// Buffered tracing-event collector for the current thread.
///
/// Construct with [`Self::install`]; access events through
/// [`Self::events`]; drop to uninstall. Single-threaded by construction:
/// the installed subscriber is per-thread (via
/// [`tracing::subscriber::set_default`]), and the inner buffer is
/// reachable only through this guard. `!Send` is structural—the
/// `Rc` and the [`tracing::dispatcher::DefaultGuard`] both inherit it.
#[must_use = "Drop the DiagnosticCapture only when you are done collecting"]
pub struct DiagnosticCapture {
    inner: Arc<Mutex<CaptureBuffer>>,
    // `tracing::subscriber::set_default` returns a guard whose `Drop`
    // restores the previous default subscriber on this thread.
    _default_guard: tracing::subscriber::DefaultGuard,
    // Mark the guard `!Send + !Sync`. The internal buffer uses
    // `Arc<Mutex<_>>` only because `tracing`'s `Subscriber` trait
    // requires `Send + Sync`; the *guard* is intentionally pinned to
    // the thread that installed it (the default-subscriber slot is
    // thread-local).
    _not_send_sync: PhantomData<*mut ()>,
}

impl DiagnosticCapture {
    /// Install a capture with the default capacity
    /// ([`DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY`]).
    ///
    /// Captures `lean_rs` events at every level for the duration of the
    /// returned guard. Events from other targets are dropped (they
    /// still pass through to any *outer* subscriber the test may have
    /// installed earlier, because `set_default` is scoped per thread).
    pub fn install() -> Self {
        Self::with_capacity(DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY)
    }

    /// Install a capture with a custom event-buffer capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let inner = Arc::new(Mutex::new(CaptureBuffer::new(capacity)));
        let layer = CaptureLayer {
            buffer: Arc::clone(&inner),
        };
        let subscriber = Registry::default().with(layer);
        let default_guard = tracing::subscriber::set_default(subscriber);
        Self {
            inner,
            _default_guard: default_guard,
            _not_send_sync: PhantomData,
        }
    }

    /// Snapshot of the captured events so far, in insertion order.
    /// Cheap clone; the capture buffer keeps accumulating after the
    /// call.
    #[must_use]
    pub fn events(&self) -> Vec<CapturedEvent> {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.events.iter().cloned().collect()
    }

    /// Number of events that were dropped because the bounded buffer
    /// was full. `0` for any test that stays under [`Self::capacity`].
    #[must_use]
    pub fn overflowed(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.overflowed
    }

    /// The buffer's capacity in events.
    #[must_use]
    pub fn capacity(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.capacity
    }
}

impl fmt::Debug for DiagnosticCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f.debug_struct("DiagnosticCapture")
            .field("events", &inner.events.len())
            .field("overflowed", &inner.overflowed)
            .finish_non_exhaustive()
    }
}

/// Internal shared state for the layer and the guard.
struct CaptureBuffer {
    events: VecDeque<CapturedEvent>,
    overflowed: usize,
    capacity: usize,
}

impl CaptureBuffer {
    fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            events: VecDeque::with_capacity(capacity),
            overflowed: 0,
            capacity,
        }
    }

    fn push(&mut self, event: CapturedEvent) {
        if self.events.len() >= self.capacity {
            self.events.pop_front();
            self.overflowed = self.overflowed.saturating_add(1);
        }
        self.events.push_back(event);
    }
}

/// `tracing` layer that pushes incoming events into the shared buffer.
struct CaptureLayer {
    buffer: Arc<Mutex<CaptureBuffer>>,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Snapshot the span name as a CapturedEvent so callers can see
        // that a span (e.g. `lean_rs.host.session.import`) was entered
        // even when no inner event fires.
        let metadata = attrs.metadata();
        if !metadata.target().starts_with("lean_rs") {
            return;
        }
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        let span_name = metadata.name();
        let event = CapturedEvent {
            target: metadata.target().to_owned(),
            level: level_str(*metadata.level()),
            name: "span_open",
            span: Some(span_name.to_owned()),
            code: visitor.code,
            message: visitor.message.unwrap_or_default(),
            fields: visitor.other_fields,
        };
        // SAFETY (logical): the layer only runs on the thread that
        // owns the `Rc<RefCell<_>>`; `set_default` is scoped per-thread.
        if let Ok(mut buf) = self.buffer.lock() {
            buf.push(event);
        }
        // The span itself is intentionally ignored after this—we do
        // not retain per-span extension storage. `ctx` is only here for
        // potential future enrichment.
        let _ = (id, ctx);
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        if !metadata.target().starts_with("lean_rs") {
            return;
        }
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let span_name = ctx
            .event_span(event)
            .map(|s| s.name().to_owned())
            .or_else(|| ctx.lookup_current().map(|s| s.name().to_owned()));
        let captured = CapturedEvent {
            target: metadata.target().to_owned(),
            level: level_str(*metadata.level()),
            name: metadata.name(),
            span: span_name,
            code: visitor.code,
            message: visitor.message.unwrap_or_default(),
            fields: visitor.other_fields,
        };
        if let Ok(mut buf) = self.buffer.lock() {
            buf.push(captured);
        }
    }
}

const fn level_str(level: tracing::Level) -> &'static str {
    match level {
        tracing::Level::ERROR => "error",
        tracing::Level::WARN => "warn",
        tracing::Level::INFO => "info",
        tracing::Level::DEBUG => "debug",
        tracing::Level::TRACE => "trace",
    }
}

/// Tracing field visitor that extracts `code`, `message`, and other
/// structured fields into a typed shape.
#[derive(Default)]
struct FieldVisitor {
    code: Option<LeanDiagnosticCode>,
    message: Option<String>,
    other_fields: Vec<(&'static str, String)>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");
        self.record_str(field, &rendered);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "code" => {
                if let Some(known) = parse_code_str(value) {
                    self.code = Some(known);
                } else {
                    self.other_fields.push(("code", value.to_owned()));
                }
            }
            "message" => self.message = Some(value.to_owned()),
            other => self.other_fields.push((other, value.to_owned())),
        }
    }
}

/// Map a `code` field's rendered value back to a [`LeanDiagnosticCode`].
///
/// Accepts both the stable id (`"lean_rs.linking"`) and the bare
/// variant name (`"Linking"`); also tolerates the `Debug` rendering
/// `tracing` produces for `&str` fields, which is `"\"...\""` (with
/// embedded quotes).
fn parse_code_str(raw: &str) -> Option<LeanDiagnosticCode> {
    let trimmed = raw.trim_matches('"');
    match trimmed {
        "lean_rs.runtime_init" | "RuntimeInit" => Some(LeanDiagnosticCode::RuntimeInit),
        "lean_rs.linking" | "Linking" => Some(LeanDiagnosticCode::Linking),
        "lean_rs.module_init" | "ModuleInit" => Some(LeanDiagnosticCode::ModuleInit),
        "lean_rs.symbol_lookup" | "SymbolLookup" => Some(LeanDiagnosticCode::SymbolLookup),
        "lean_rs.abi_conversion" | "AbiConversion" => Some(LeanDiagnosticCode::AbiConversion),
        "lean_rs.lean_exception" | "LeanException" => Some(LeanDiagnosticCode::LeanException),
        "lean_rs.elaboration" | "Elaboration" => Some(LeanDiagnosticCode::Elaboration),
        "lean_rs.unsupported" | "Unsupported" => Some(LeanDiagnosticCode::Unsupported),
        "lean_rs.internal" | "Internal" => Some(LeanDiagnosticCode::Internal),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::{info, info_span};

    #[test]
    fn captures_lean_rs_event() {
        let capture = DiagnosticCapture::install();
        info!(target: "lean_rs", code = "lean_rs.linking", "linker failed");
        let events = capture.events();
        assert!(
            events
                .iter()
                .any(|e| e.code == Some(LeanDiagnosticCode::Linking) && e.message == "linker failed"),
            "expected one linking event, got {events:?}",
        );
    }

    #[test]
    fn ignores_other_targets() {
        let capture = DiagnosticCapture::install();
        info!(target: "some_other_crate", "boring");
        assert!(capture.events().is_empty());
    }

    #[test]
    fn captures_span_open() {
        let capture = DiagnosticCapture::install();
        let _g = info_span!(target: "lean_rs", "lean_rs.host.session.import").entered();
        let events = capture.events();
        assert!(
            events
                .iter()
                .any(|e| e.span.as_deref() == Some("lean_rs.host.session.import")),
            "expected a span_open record, got {events:?}",
        );
    }

    #[test]
    fn bounded_buffer_drops_oldest() {
        let capture = DiagnosticCapture::with_capacity(2);
        info!(target: "lean_rs", "one");
        info!(target: "lean_rs", "two");
        info!(target: "lean_rs", "three");
        let events = capture.events();
        assert_eq!(events.len(), 2);
        let messages: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, ["two", "three"]);
        assert_eq!(capture.overflowed(), 1);
    }
}
