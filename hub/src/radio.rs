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

/// While a camera streams, no search starts: a scan takes radio time from
/// Wi-Fi, and every gap in the video measured on the bench (over 0.6 s, the
/// heater and fridge out of range) fell during one. The heater's search waits
/// too: the video only streams while someone has the Cameras page open, so the
/// wait ends within VIDEO_HOLD of leaving it. Each streaming announcement
/// extends the wait by VIDEO_HOLD, so a camera that drops off cannot hold the
/// radio. Links already connected are not affected.
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

/// Waits until no camera is streaming and no other link is searching or
/// connecting, then takes the turn.
pub async fn take(timer: &mut EspAsyncTimer) -> anyhow::Result<Turn> {
    let mut paused_logged = false;
    loop {
        if video_active() {
            if !paused_logged {
                log::info!("Bluetooth search waits: a camera is streaming");
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
