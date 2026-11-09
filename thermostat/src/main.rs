//! M-Pulse thermostat node.
//!
//! Step 1 scaffold: samples the DHT11 on a background thread and periodically
//! scans for BLE devices so we can identify the diesel heater. The heater
//! client and the thermostat state machine build on top of this.

mod ble;
mod sensor;

use std::time::Duration;

use esp32_nimble::BLEDevice;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::timer::EspTaskTimerService;
use log::{error, info, warn};

/// Length of each BLE scan.
const SCAN_DURATION_MS: i32 = 10_000;

/// Pause between scans.
const SCAN_PAUSE: Duration = Duration::from_secs(20);

/// A reading older than this is treated as missing.
const READING_MAX_AGE: Duration = Duration::from_secs(60);

fn main() -> anyhow::Result<()> {
    // Required once at startup so the ESP-IDF runtime patches are linked in.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("M-Pulse thermostat node starting");

    let peripherals = Peripherals::take()?;

    // DHT11 DATA is wired to GPIO4. Change the pin here if the wiring changes.
    let reading = sensor::spawn(peripherals.pins.gpio4)?;

    let timers = EspTaskTimerService::new()?;
    let ble_device = BLEDevice::take();

    block_on(run(ble_device, &timers, reading))
}

/// Main loop: scan for BLE devices, report the cabin reading, pause, repeat.
async fn run(
    ble_device: &BLEDevice,
    timers: &EspTaskTimerService,
    reading: sensor::SharedReading,
) -> anyhow::Result<()> {
    let mut timer = timers.timer_async()?;

    loop {
        match ble::scan(ble_device, SCAN_DURATION_MS).await {
            Ok(found) => ble::log_scan(&found),
            Err(err) => error!("BLE scan failed: {err}"),
        }

        let latest = *reading.lock().unwrap_or_else(|e| e.into_inner());
        match latest {
            Some(r) if r.age() <= READING_MAX_AGE => info!(
                "Cabin: {:.0} °C, {:.0} % RH ({} s old)",
                r.temperature_c,
                r.humidity_pct,
                r.age().as_secs()
            ),
            Some(_) => warn!("Cabin: last sensor reading is stale"),
            None => warn!("Cabin: no sensor reading yet"),
        }

        timer.after(SCAN_PAUSE).await?;
    }
}
