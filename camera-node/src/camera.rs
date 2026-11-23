//! The OV3660 on the Freenove ESP32-WROVER camera board.
//!
//! The board has no power-down pin, so standby means stopping the driver:
//! the camera clock stops and the sensor stops producing frames. The sensor
//! is still on the 3.3 V rail in standby, so its exact current draw is
//! measured, not assumed.

use anyhow::{bail, Context, Result};
use esp_idf_svc::sys::{self, camera};
use log::info;
use m_pulse_core::camera::CameraState;

use crate::state::{self, Settings, Size};

/// Starts the camera with this board's pinout and returns once the sensor answers.
/// The frame size and JPEG quality are the ones chosen by `POST /settings`.
pub fn power_up() -> Result<()> {
    let settings = state::settings();
    let mut config: camera::camera_config_t = unsafe { std::mem::zeroed() };
    config.pin_pwdn = -1;
    config.pin_reset = -1;
    config.pin_xclk = 21;
    config.__bindgen_anon_1.pin_sccb_sda = 26;
    config.__bindgen_anon_2.pin_sccb_scl = 27;
    config.pin_d7 = 35;
    config.pin_d6 = 34;
    config.pin_d5 = 39;
    config.pin_d4 = 36;
    config.pin_d3 = 19;
    config.pin_d2 = 18;
    config.pin_d1 = 5;
    config.pin_d0 = 4;
    config.pin_vsync = 25;
    config.pin_href = 23;
    config.pin_pclk = 22;
    config.xclk_freq_hz = 20_000_000;
    config.ledc_timer = sys::ledc_timer_t_LEDC_TIMER_0;
    config.ledc_channel = sys::ledc_channel_t_LEDC_CHANNEL_0;
    config.pixel_format = camera::pixformat_t_PIXFORMAT_JPEG;
    config.frame_size = settings.size.framesize();
    config.jpeg_quality = i32::from(settings.quality);
    config.fb_count = 2;
    config.fb_location = camera::camera_fb_location_t_CAMERA_FB_IN_PSRAM;
    config.grab_mode = camera::camera_grab_mode_t_CAMERA_GRAB_LATEST;

    let err = unsafe { camera::esp_camera_init(&config) };
    if err != sys::ESP_OK {
        bail!("camera init failed: {err}");
    }
    let sensor = unsafe { camera::esp_camera_sensor_get() };
    if !sensor.is_null() {
        info!("Camera sensor PID 0x{:04x}", unsafe { (*sensor).id.PID });
    }
    Ok(())
}

/// Stops the camera clock and driver. Callers must hold the write lock.
pub fn power_down() {
    unsafe { camera::esp_camera_deinit() };
}

/// Applies new settings to a streaming camera. The caller must hold the write
/// lock. Quality changes on the running sensor. A new frame size restarts the
/// driver, because the driver sizes its frame buffers when it starts.
pub fn change_settings(size: Size, quality: u8) -> Result<Settings> {
    let old = state::settings();
    let new = Settings { size, quality };

    if new.size != old.size {
        // The restart reads the stored settings, so store the new ones first.
        state::set_settings(new);
        power_down();
        if let Err(err) = power_up() {
            state::set_settings(old);
            state::set_state(CameraState::Standby);
            crate::net::set_power_save(true);
            return Err(err.context("restart with the new frame size failed; the camera is now in standby"));
        }
    } else {
        let sensor = unsafe { camera::esp_camera_sensor_get() };
        anyhow::ensure!(!sensor.is_null(), "no camera sensor");
        let set_quality = unsafe { (*sensor).set_quality }.context("sensor has no set_quality")?;
        let ret = unsafe { set_quality(sensor, i32::from(quality)) };
        anyhow::ensure!(ret == 0, "set JPEG quality {quality} failed: error {ret}; settings unchanged");
        state::set_settings(new);
    }

    info!("Settings: {} quality {}", new.size.name(), new.quality);
    Ok(new)
}

/// One JPEG frame, returned to the driver on drop.
pub struct Frame(*mut camera::camera_fb_t);

impl Frame {
    /// Grabs the latest frame, or `None` if the driver has no frame ready.
    pub fn grab() -> Option<Frame> {
        let fb = unsafe { camera::esp_camera_fb_get() };
        (!fb.is_null()).then_some(Frame(fb))
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts((*self.0).buf, (*self.0).len) }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { camera::esp_camera_fb_return(self.0) };
    }
}
