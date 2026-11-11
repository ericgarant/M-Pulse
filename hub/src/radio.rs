//! Takes turns on the Bluetooth radio.
//!
//! The heater, fridge and Bluetti links run side by side, but NimBLE allows one scan
//! or one connection attempt at a time. Each link holds this turn while it
//! searches and connects, and releases it once connected.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use esp_idf_svc::timer::EspAsyncTimer;

static BUSY: AtomicBool = AtomicBool::new(false);

/// Scan timing, in 0.625 ms units: listen 30 ms out of every 100 ms.
///
/// Wi-Fi and Bluetooth share the S3's one radio. A scan that listens all the
/// time (the old window of 99 out of 100) left Wi-Fi almost no airtime, so
/// while a device was out of range the dashboard stalled for seconds in every
/// search. At 30 % the heater, fridge and Bluetti are still found within a
/// few seconds, since they advertise several times a second.
pub const SCAN_INTERVAL: u16 = 160;
pub const SCAN_WINDOW: u16 = 48;

/// Holds the radio until dropped.
pub struct Turn(());

impl Drop for Turn {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// Waits until no other link is searching or connecting, then takes the turn.
pub async fn take(timer: &mut EspAsyncTimer) -> anyhow::Result<Turn> {
    loop {
        if BUSY
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Ok(Turn(()));
        }
        timer.after(Duration::from_millis(200)).await?;
    }
}
