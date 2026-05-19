//! ESP32 diagnostic event collector.

use core::cell::RefCell;

use critical_section::Mutex;
use muninn_gate_core::{
    DiagnosticCollector,
    DiagnosticEvent,
    DiagnosticLevel,
    DiagnosticSnapshot,
    DiagnosticSubsystem,
    Error,
    FixedDiagnosticRing,
};

/// Number of diagnostic events retained by the current ESP32 bring-up build.
pub const ESP32_DIAGNOSTIC_EVENT_LIMIT: usize = 32;

static DIAGNOSTICS: Mutex<RefCell<FixedDiagnosticRing<ESP32_DIAGNOSTIC_EVENT_LIMIT>>> =
    Mutex::new(RefCell::new(FixedDiagnosticRing::new()));

/// Record one diagnostic event in the ESP32 platform collector.
pub fn record(
    timestamp_ms: u64,
    level: DiagnosticLevel,
    subsystem: DiagnosticSubsystem,
    code: &str,
    message: &str,
) -> Result<(), Error>
{
    let event = DiagnosticEvent::new(timestamp_ms, level, subsystem, code, message)?;
    critical_section::with(|cs| DIAGNOSTICS.borrow_ref_mut(cs).record(event))
}

/// Return a retained diagnostic snapshot.
pub fn snapshot() -> DiagnosticSnapshot<ESP32_DIAGNOSTIC_EVENT_LIMIT>
{
    critical_section::with(|cs| DIAGNOSTICS.borrow_ref(cs).snapshot())
}
