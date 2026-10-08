//! Opt-in phase telemetry for resident Language Service startup.
//!
//! Set `MNLS_STARTUP_PROFILE=1` to emit one JSON object per measured phase to
//! stderr. This stays disabled for ordinary service operation.

use std::sync::OnceLock;

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("MNLS_STARTUP_PROFILE").is_some_and(|value| value == "1"))
}

pub(crate) fn emit(event: &str, fields: serde_json::Value) {
    if !enabled() {
        return;
    }
    eprintln!(
        "mnls-startup-profile: {}",
        serde_json::json!({
            "schema": "mnls.resident-startup-profile/1",
            "event": event,
            "fields": fields,
        })
    );
}

pub(crate) fn elapsed_us(started: std::time::Instant) -> u64 {
    started.elapsed().as_micros().min(u64::MAX as u128) as u64
}
