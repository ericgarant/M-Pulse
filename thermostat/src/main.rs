//! M-Pulse thermostat node.
//!
//! Step 2, heater link: samples the DHT11 on a background thread, finds the
//! BougeRV diesel heater over BLE, performs the vendor app's opening
//! handshake and logs the heater's status reports. It does not yet switch the
//! heater on or off. The thermostat state machine builds on this.

mod ble;
mod control;
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

/// Turns on debug logging for the BLE stack, to see what happens on the
/// heater link right before it goes quiet. Very chatty; switch off once the
/// link is understood.
const BLE_DEBUG: bool = true;

/// A reading older than this is treated as missing.
const READING_MAX_AGE: Duration = Duration::from_secs(60);

fn main() -> anyhow::Result<()> {
    // Required once at startup so the ESP-IDF runtime patches are linked in.
    esp_idf_svc::sys::link_patches();
    // Installs the logger; this must happen only once.
    let logger = esp_idf_svc::log::init_from_esp_idf();
    if BLE_DEBUG {
        enable_ble_debug_logging(logger);
    }

    // Reference point for the build-time clock; see `heater::current_time`.
    heater::mark_boot();

    info!("M-Pulse thermostat node starting");

    let peripherals = Peripherals::take()?;

    // DHT11 DATA is wired to GPIO21. Change the pin here if the wiring changes.
    let reading = sensor::spawn(peripherals.pins.gpio21)?;

    // The thermostat decides independently of the BLE link. For now it only
    // logs what it would do.
    control::spawn(reading.clone(), m_pulse_core::thermostat::Config::default())?;

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

/// Raises the log level for the BLE client (connection update requests and
/// unhandled GAP events) and the NimBLE host (L2CAP and GAP procedures).
/// Everything else stays at info.
fn enable_ble_debug_logging(logger: &esp_idf_svc::log::EspLogger) {
    log::set_max_level(log::LevelFilter::Debug);
    for target in ["esp32_nimble::client::ble_client", "NimBLE"] {
        if let Err(err) = logger
            .filter()
            .set_target_level(target, log::LevelFilter::Debug)
        {
            warn!("Could not raise log level for {target}: {err}");
        }
    }
    info!("BLE debug logging on");
}
