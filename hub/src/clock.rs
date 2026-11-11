//! Wall-clock time for the hub.
//!
//! The board has no battery-backed clock, so the time is unknown at boot. It
//! is set by SNTP once the hub reaches the internet through your Wi-Fi, or by
//! the dashboard, which sends the phone's time when the hub has none.

use std::time::{SystemTime, UNIX_EPOCH};

use m_pulse_core::clock::{LocalTime, Zone};

/// Time zone for the heater's clock.
pub const ZONE: Zone = Zone::US_EASTERN;

/// Anything before 2025-01-01 means the clock was never set.
const EARLIEST_VALID: i64 = 1_735_689_600;

/// Current Unix time in seconds, or `None` if the clock has not been set.
pub fn unix_now() -> Option<i64> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    (secs >= EARLIEST_VALID).then_some(secs)
}

/// Current local time, or `None` if the clock has not been set.
pub fn local_now() -> Option<LocalTime> {
    ZONE.local(unix_now()?)
}

/// Sets the system clock to `secs` (Unix time).
pub fn set_unix(secs: i64) -> anyhow::Result<()> {
    anyhow::ensure!(secs >= EARLIEST_VALID, "time {secs} is too early");
    let tv = esp_idf_svc::sys::timeval {
        tv_sec: secs as _,
        tv_usec: 0,
    };
    // SAFETY: plain libc call with a valid timeval and no time zone.
    let rc = unsafe { esp_idf_svc::sys::settimeofday(&tv, core::ptr::null()) };
    anyhow::ensure!(rc == 0, "settimeofday failed ({rc})");
    Ok(())
}
