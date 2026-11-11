//! M-Pulse hub (ESP32-S3).
//!
//! Owns the BLE link to the BougeRV diesel heater, receives cabin readings
//! from the sensor node over Wi-Fi, runs the thermostat, and serves the
//! dashboard.
//!
//! After every restart the hub is in Monitor mode and sends no switch
//! commands. The heater is switched only from the dashboard's On and Off
//! buttons, or by the thermostat once Auto mode is chosen there.
//!
//! The classic ESP32 could not hold this heater's BLE link (the heater went
//! silent about 11 s into every connection), while the S3 holds it, so the
//! heater link lives here.

mod cabin;
mod clock;
mod control;
mod heater;
mod net;
mod state;
mod web;

use esp32_nimble::BLEDevice;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::EspSntp;
use esp_idf_svc::timer::EspTaskTimerService;
use log::info;
use m_pulse_core::thermostat::Config;

fn main() -> anyhow::Result<()> {
    // Required once at startup so the ESP-IDF runtime patches are linked in.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("M-Pulse hub starting");

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let state = state::Shared::new(Config::default());

    net::start(peripherals.modem, sysloop, nvs, state.clone())?;
    // Sets the clock whenever the hub reaches the internet. Kept alive for
    // the life of the firmware.
    let _sntp = EspSntp::new_default()?;

    cabin::spawn(state.clone())?;
    control::spawn(state.clone())?;
    let _server = web::start(state.clone())?;

    let timers = EspTaskTimerService::new()?;
    let mut timer = timers.timer_async()?;
    let ble = BLEDevice::take();

    block_on(heater::run(ble, &mut timer, state))
}
