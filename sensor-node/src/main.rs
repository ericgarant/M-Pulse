//! M-Pulse sensor node (classic ESP32 with a DHT11 on GPIO21).
//!
//! Samples the cabin temperature and humidity and broadcasts each reading to
//! the hub over Wi-Fi as a UDP packet (see `m_pulse_core::wire`). It joins
//! your Wi-Fi when that is configured and in range, and otherwise the hub's
//! own network, so it works with or without a router.
//!
//! The heater link lives on the hub: this board's Bluetooth could not hold
//! the heater's connection.

mod net;
mod sensor;

use std::net::{Ipv4Addr, UdpSocket};
use std::thread;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::{info, warn};
use m_pulse_core::wire::{self, CabinPacket};

/// How often a reading is sent. Matches the sensor's sample rate.
const SEND_INTERVAL: Duration = Duration::from_secs(5);

/// Readings older than this are not sent; the hub then notices the sensor
/// is lost and keeps the heater off.
const MAX_AGE: Duration = Duration::from_secs(30);

fn main() -> anyhow::Result<()> {
    // Required once at startup so the ESP-IDF runtime patches are linked in.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("M-Pulse sensor node starting");

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // DHT11 DATA is wired to GPIO21, with a 10k pull-up to 3.3V.
    let reading = sensor::spawn(peripherals.pins.gpio21)?;

    let online = net::start(peripherals.modem, sysloop, nvs)?;

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    let target = (Ipv4Addr::BROADCAST, wire::PORT);

    let mut seq: u32 = 0;
    loop {
        thread::sleep(SEND_INTERVAL);

        let latest = *reading.lock().unwrap_or_else(|e| e.into_inner());
        let Some(r) = latest.filter(|r| r.age() <= MAX_AGE) else {
            warn!("No fresh reading to send");
            continue;
        };
        if !online.is_up() {
            continue;
        }

        seq = seq.wrapping_add(1);
        let packet = CabinPacket {
            seq,
            temperature_c: r.temperature_c,
            humidity_pct: r.humidity_pct,
        };
        if let Err(err) = socket.send_to(packet.encode().as_bytes(), target) {
            warn!("Sending reading #{seq} failed: {err}");
        }
    }
}
