//! Camera state shared between the control threads, the camera driver and
//! the HTTP server.
//!
//! The camera driver is only valid while it is started, so frames are read
//! under [`CAMERA`]'s read lock and starting or stopping takes the write lock.
//! The driver is never shut down while a frame is still in use.

use std::ops::RangeInclusive;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use esp_idf_svc::sys::camera;
use m_pulse_core::camera::CameraState;

/// Read-locked while a frame is in use; write-locked to start or stop the driver.
pub static CAMERA: RwLock<()> = RwLock::new(());

static STATE: Mutex<CameraState> = Mutex::new(CameraState::Standby);
static LAST_VIEWED: Mutex<Option<Instant>> = Mutex::new(None);

/// Number of open `/stream` connections.
pub static VIEWERS: AtomicUsize = AtomicUsize::new(0);

/// Most viewers allowed at once. The stream is per-viewer work, so keep this small.
pub const MAX_VIEWERS: usize = 2;

/// How long a streaming camera waits with no viewer before it goes to standby.
/// This is only a backup for a camera the hub forgot to stop: the hub's stop
/// command is the normal way to sleep, so this is kept long enough that a
/// short gap between viewers does not wake the camera up and down.
pub const IDLE_OFF: Duration = Duration::from_secs(5 * 60);

/// Frame sizes the `/settings` route accepts, the four the OV3660 driver is
/// used with here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Vga,
    Svga,
    Xga,
    Sxga,
}

impl Size {
    pub fn parse(text: &str) -> Option<Size> {
        match text {
            "VGA" => Some(Size::Vga),
            "SVGA" => Some(Size::Svga),
            "XGA" => Some(Size::Xga),
            "SXGA" => Some(Size::Sxga),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Size::Vga => "VGA",
            Size::Svga => "SVGA",
            Size::Xga => "XGA",
            Size::Sxga => "SXGA",
        }
    }

    pub fn framesize(self) -> camera::framesize_t {
        match self {
            Size::Vga => camera::framesize_t_FRAMESIZE_VGA,
            Size::Svga => camera::framesize_t_FRAMESIZE_SVGA,
            Size::Xga => camera::framesize_t_FRAMESIZE_XGA,
            Size::Sxga => camera::framesize_t_FRAMESIZE_SXGA,
        }
    }
}

/// Frame size and JPEG quality. Lower quality numbers give better JPEGs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub size: Size,
    pub quality: u8,
}

/// JPEG quality values the `/settings` route accepts.
pub const QUALITY_RANGE: RangeInclusive<u8> = 10..=40;

/// Chosen by `POST /settings` and read by `power_up()`, so the next start uses
/// them. Starts at the values the camera booted with.
// SVGA q12: sharper than VGA and smoother than XGA in the bench tests.
pub static SETTINGS: Mutex<Settings> = Mutex::new(Settings {
    size: Size::Svga,
    quality: 12,
});

pub fn settings() -> Settings {
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn set_settings(settings: Settings) {
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = settings;
}

pub fn state() -> CameraState {
    *STATE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn set_state(state: CameraState) {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = state;
}

/// Records that someone is watching (or has just been started), which resets the idle timer.
pub fn touch_viewed() {
    *LAST_VIEWED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

/// How long since anyone last watched or the camera was started.
pub fn idle_for() -> Duration {
    LAST_VIEWED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .map(|t| t.elapsed())
        .unwrap_or(Duration::ZERO)
}

/// Takes the read lock. Reads never fail; a poisoned lock is still usable here.
pub fn read_camera() -> RwLockReadGuard<'static, ()> {
    CAMERA.read().unwrap_or_else(|e| e.into_inner())
}
