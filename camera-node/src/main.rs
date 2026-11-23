//! M-Pulse camera node (ESP32-WROVER with an OV3660).
//!
//! Sits in standby with the camera off. The hub asks it to start over UDP,
//! it streams MJPEG at `/stream` and a single frame at `/still.jpg`, and it
//! returns to standby when the hub stops it or, as a backup, nobody has
//! watched for 5 minutes. `/settings` sets the frame size and JPEG quality.
//! It announces its name and state to the hub every few seconds.

mod camera;
mod control;
mod net;
mod server;
mod state;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;
use log::info;
use m_pulse_core::camera::valid_name;

/// This camera's name on the hub's dashboard, set at build time with
/// `M_PULSE_CAMERA_NAME` (for example `camper` or `cab`).
const NAME: &str = match option_env!("M_PULSE_CAMERA_NAME") {
    Some(name) => name,
    None => "camper",
};

fn main() -> anyhow::Result<()> {
    // Required once at startup so the ESP-IDF runtime patches are linked in.
    sys::link_patches();
    let logger = esp_idf_svc::log::init_from_esp_idf();
    // A viewer leaving mid-stream makes the HTTP server log a warning it
    // cannot avoid (the closing chunk fails on a dead socket). The stream
    // itself is handled, so keep those warnings out of the log.
    logger
        .filter()
        .set_target_level("esp_idf_svc::http::server", log::LevelFilter::Error)?;
    // Harmless C-level warnings: socket errors after a viewer leaves, and
    // the LEDC pin reservation that survives a restart of the camera clock.
    for tag in [c"httpd_txrx", c"ledc"] {
        unsafe { sys::esp_log_level_set(tag.as_ptr(), sys::esp_log_level_t_ESP_LOG_ERROR) };
    }

    anyhow::ensure!(valid_name(NAME), "invalid camera name \"{NAME}\"");
    info!("M-Pulse camera node \"{NAME}\" starting, camera in standby");
    info!("Reset reason: {}", reset_reason());

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let online = net::start(peripherals.modem, sysloop, nvs)?;
    control::spawn(NAME, online)?;
    let _servers = server::start()?;
    info!("HTTP up: port 80 (page, still, settings), port {} (stream)", server::STREAM_PORT);

    // Everything runs on background threads; keep the main task alive.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

/// Why the chip last restarted, for the boot log. A brownout points to the
/// power supply.
fn reset_reason() -> String {
    let reason = unsafe { sys::esp_reset_reason() };
    let name = match reason {
        sys::esp_reset_reason_t_ESP_RST_POWERON => "power on",
        sys::esp_reset_reason_t_ESP_RST_EXT => "reset pin",
        sys::esp_reset_reason_t_ESP_RST_SW => "software restart",
        sys::esp_reset_reason_t_ESP_RST_PANIC => "panic",
        sys::esp_reset_reason_t_ESP_RST_INT_WDT => "interrupt watchdog",
        sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "task watchdog",
        sys::esp_reset_reason_t_ESP_RST_WDT => "other watchdog",
        sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => "wake from deep sleep",
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "brownout (the supply voltage dropped)",
        _ => return format!("code {reason}"),
    };
    name.to_string()
}
