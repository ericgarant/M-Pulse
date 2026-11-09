//! M-Pulse thermostat node.
//!
//! Step 2, heater discovery: samples the DHT11 on a background thread, finds
//! the BougeRV diesel heater over BLE, and logs its GATT table and any
//! notifications it sends. The thermostat state machine builds on this.

mod ble;
mod heater;
mod sensor;

use std::time::Duration;

use esp32_nimble::BLEDevice;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::timer::EspTaskTimerService;
use log::{error, info, warn};

/// How long to look for the heater in each cycle.
const FIND_DURATION_MS: i32 = 15_000;

/// How long to stay connected and log notifications.
const LISTEN_DURATION: Duration = Duration::from_secs(90);

/// Length of the general scan that runs when the heater is not found.
const SCAN_DURATION_MS: i32 = 10_000;

/// Pause between cycles.
const CYCLE_PAUSE: Duration = Duration::from_secs(20);

/// A reading older than this is treated as missing.
const READING_MAX_AGE: Duration = Duration::from_secs(60);

fn main() -> anyhow::Result<()> {
    // Required once at startup so the ESP-IDF runtime patches are linked in.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("M-Pulse thermostat node starting");

    let peripherals = Peripherals::take()?;

    // DHT11 DATA is wired to GPIO21. Change the pin here if the wiring changes.
    let reading = sensor::spawn(peripherals.pins.gpio21)?;

    let timers = EspTaskTimerService::new()?;
    let ble_device = BLEDevice::take();

    block_on(run(ble_device, &timers, reading))
}

/// Main loop: find the heater, inspect it, report the cabin reading, pause.
async fn run(
    ble_device: &BLEDevice,
    timers: &EspTaskTimerService,
    reading: sensor::SharedReading,
) -> anyhow::Result<()> {
    let mut timer = timers.timer_async()?;

    loop {
        match heater::find(ble_device, FIND_DURATION_MS).await {
            Ok(Some(addr)) => {
                if let Err(err) =
                    heater::inspect(ble_device, &addr, LISTEN_DURATION, &mut timer).await
                {
                    error!("Heater inspection failed: {err}");
                }
            }
            Ok(None) => {
                warn!("Heater not found. Listing everything in range instead");
                match ble::scan(ble_device, SCAN_DURATION_MS).await {
                    Ok(found) => ble::log_scan(&found),
                    Err(err) => error!("BLE scan failed: {err}"),
                }
            }
            Err(err) => error!("Heater search failed: {err}"),
        }

        log_cabin(&reading);
        timer.after(CYCLE_PAUSE).await?;
    }
}

fn log_cabin(reading: &sensor::SharedReading) {
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
}
