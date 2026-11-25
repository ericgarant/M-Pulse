//! M-Pulse hub (ESP32-S3).
//!
//! Owns the BLE links to the BougeRV diesel heater, the BougeRV Rocky 50
//! fridge and the Bluetti Elite 200 V2 power station, reads the cabin from its
//! own SHT3x (falling back to the sensor node over Wi-Fi), runs the
//! thermostat, starts the cameras when a viewer asks, and serves the
//! dashboard.
//!
//! After every restart the hub is in Monitor mode and the thermostat sends
//! no switch commands. The heater is switched only from the dashboard's On
//! and Off buttons, by the thermostat once Auto mode is chosen there, or
//! switched off by the low-battery stop when the Bluetti runs low, or by the
//! fuel reserve when the hub's fuel estimate says the tank is nearly empty.
//!
//! The classic ESP32 could not hold this heater's BLE link (the heater went
//! silent about 11 s into every connection), while the S3 holds it, so the
//! heater link lives here.

mod bluetti;
mod cabin;
mod camera;
mod clock;
mod control;
mod fridge;
mod heater;
mod net;
mod radio;
mod sht3x;
mod state;
mod store;
mod web;

use embassy_futures::join::join3;
use esp32_nimble::BLEDevice;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::EspSntp;
use esp_idf_svc::timer::EspTaskTimerService;
use log::{info, warn};
use m_pulse_core::thermostat::Config;
use m_pulse_core::{fuel, rules, thermostat};

fn main() -> anyhow::Result<()> {
    // Required once at startup so the ESP-IDF runtime patches are linked in.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("M-Pulse hub starting");

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let state = state::Shared::new(Config::default());

    // The fuel gauge lives in flash. Without it the gauge still works, but
    // starts unknown after every restart.
    let fuel_store = match store::Store::<fuel::Saved>::open(nvs.clone()) {
        Ok((store, saved)) => {
            if let Some(saved) = saved {
                let mut s = state.lock();
                s.fuel.config = saved.config;
                s.fuel.gauge = saved.gauge;
            }
            Some(store)
        }
        Err(err) => {
            warn!("Fuel gauge: flash not available, it will not survive a restart: {err}");
            None
        }
    };

    // The battery rules' levels and switches live in flash too. Without it
    // they go back to the defaults after every restart.
    let rules_store = match store::Store::<rules::Saved>::open(nvs.clone()) {
        Ok((store, saved)) => {
            if let Some(saved) = saved {
                let mut s = state.lock();
                s.cutoff.config = saved.cutoff;
                s.saver.config = saved.saver;
            }
            Some(store)
        }
        Err(err) => {
            warn!("Battery rules: flash not available, they will not survive a restart: {err}");
            None
        }
    };

    // The comfort band's width lives in flash too. Without it the band goes
    // back to ±1 °C after every restart.
    let thermo_store = match store::Store::<thermostat::Saved>::open(nvs.clone()) {
        Ok((store, saved)) => {
            if let Some(saved) = saved {
                let mut s = state.lock();
                s.control.config = saved.apply(s.control.config);
            }
            Some(store)
        }
        Err(err) => {
            warn!("Thermostat: flash not available, the comfort band will not survive a restart: {err}");
            None
        }
    };

    // The camera rotations live in flash too. Without it they go back to 0
    // after every restart.
    let camera_store = match store::Store::<m_pulse_core::camera::Saved>::open(nvs.clone()) {
        Ok((store, saved)) => {
            if let Some(saved) = saved {
                state.cameras().restore(&saved);
            }
            Some(store)
        }
        Err(err) => {
            warn!("Cameras: flash not available, rotations will not survive a restart: {err}");
            None
        }
    };

    net::start(peripherals.modem, sysloop, nvs, state.clone())?;
    // Sets the clock whenever the hub reaches the internet. Kept alive for
    // the life of the firmware.
    let _sntp = EspSntp::new_default()?;

    // SHT3x on I2C: SDA on GPIO4 (the old DHT11 data wire), SCL on GPIO5.
    // Both are free on any ESP32-S3 module: not strapping, flash, PSRAM or
    // USB pins. The breakout board carries its own pull-ups.
    sht3x::spawn(
        peripherals.i2c0,
        peripherals.pins.gpio4,
        peripherals.pins.gpio5,
        state.clone(),
    )?;
    cabin::spawn(state.clone())?;
    camera::spawn(state.clone())?;
    control::spawn(
        state.clone(),
        fuel_store,
        rules_store,
        thermo_store,
        camera_store,
    )?;
    let _server = web::start(state.clone())?;

    let timers = EspTaskTimerService::new()?;
    let mut heater_timer = timers.timer_async()?;
    let mut fridge_timer = timers.timer_async()?;
    let mut bluetti_timer = timers.timer_async()?;
    let ble = BLEDevice::take();

    // The links run on this task and take turns to search and connect.
    let (heater, fridge, bluetti) = block_on(join3(
        heater::run(ble, &mut heater_timer, state.clone()),
        fridge::run(ble, &mut fridge_timer, state.clone()),
        bluetti::run(ble, &mut bluetti_timer, state),
    ));
    heater.and(fridge).and(bluetti)
}
