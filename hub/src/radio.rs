//! Takes turns on the Bluetooth radio.
//!
//! The heater, fridge and Bluetti links run side by side, but NimBLE allows one scan
//! or one connection attempt at a time. Each link holds this turn while it
//! searches and connects, and releases it once connected.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use esp_idf_svc::timer::EspAsyncTimer;

static BUSY: AtomicBool = AtomicBool::new(false);

/// While a camera streams, scans for the fridge and the Bluetti wait, because
/// a scan takes radio time from Wi-Fi and cut the video to a quarter in bench
/// tests. Each streaming announcement extends the pause by VIDEO_HOLD, so a
/// camera that drops off cannot hold the radio: scans resume within VIDEO_HOLD.
static VIDEO_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
const VIDEO_HOLD: Duration = Duration::from_secs(15);

/// Called for each camera announcement that says it is streaming.
pub fn video_seen() {
    *VIDEO_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + VIDEO_HOLD);
}

fn video_active() -> bool {
    VIDEO_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|until| Instant::now() < until)
}

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

/// Longest a scan waits for a streaming camera; past this it runs anyway.
const VIDEO_WAIT_MAX: Duration = Duration::from_secs(60);

/// Waits until no other link is searching or connecting, then takes the turn.
/// While a camera streams, waits up to VIDEO_WAIT_MAX before scanning anyway.
/// For the fridge and the Bluetti.
pub async fn take(timer: &mut EspAsyncTimer) -> anyhow::Result<Turn> {
    take_inner(timer, true).await
}

/// The heater's turn: never held back for a camera, because the thermostat
/// and the low-battery stop depend on the heater's link.
pub async fn take_now(timer: &mut EspAsyncTimer) -> anyhow::Result<Turn> {
    take_inner(timer, false).await
}

async fn take_inner(timer: &mut EspAsyncTimer, yield_to_video: bool) -> anyhow::Result<Turn> {
    let started = Instant::now();
    let mut paused_logged = false;
    loop {
        let hold = yield_to_video && video_active() && started.elapsed() < VIDEO_WAIT_MAX;
        if hold {
            if !paused_logged {
                log::info!("Bluetooth scan waits (up to 60 s): a camera is streaming");
                paused_logged = true;
            }
        } else if BUSY
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Ok(Turn(()));
        }
        timer.after(Duration::from_millis(200)).await?;
    }
}
