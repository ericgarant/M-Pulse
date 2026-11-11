//! Takes turns on the Bluetooth radio.
//!
//! The heater and fridge links run side by side, but NimBLE allows one scan
//! or one connection attempt at a time. Each link holds this turn while it
//! searches and connects, and releases it once connected.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use esp_idf_svc::timer::EspAsyncTimer;

static BUSY: AtomicBool = AtomicBool::new(false);

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
