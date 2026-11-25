//! Camera state shared between the control threads, the camera driver and
//! the HTTP server.
//!
//! The camera driver is only valid while it is started, so frames are read
//! under [`CAMERA`]'s read lock and starting or stopping takes the write lock.
//! The driver is never shut down while a frame is still in use.

use std::ops::RangeInclusive;
use std::sync::atomic::AtomicUsize;
use std::sync::{Condvar, Mutex, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use esp_idf_svc::sys::camera;
use m_pulse_core::camera::CameraState;

/// Read-locked while a frame is in use; write-locked to start or stop the driver.
pub static CAMERA: RwLock<()> = RwLock::new(());

static STATE: Mutex<CameraState> = Mutex::new(CameraState::Standby);
/// Notified by [`set_state`].
static STATE_CHANGED: Condvar = Condvar::new();
static LAST_VIEWED: Mutex<Option<Instant>> = Mutex::new(None);
/// When the running start's command arrived, until its first frame is served.
static STARTED_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Number of open `/stream` connections.
pub static VIEWERS: AtomicUsize = AtomicUsize::new(0);

/// Most viewers allowed at once. The stream server handles one request at a
/// time, so a second viewer waits until the first leaves; one is the real limit.
pub const MAX_VIEWERS: usize = 1;

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
// VGA q12: with two cameras streaming to a phone through the hub, VGA ran at
// about 11 to 19 frames a second each, SVGA at 4 to 6 (November 2026).
pub static SETTINGS: Mutex<Settings> = Mutex::new(Settings {
    size: Size::Vga,
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
    STATE_CHANGED.notify_all();
}

/// Waits until the state is not `seen`, or `timeout` passes, and returns the
/// state. Lets the announce thread tell the hub about a change at once.
pub fn wait_for_change(seen: CameraState, timeout: Duration) -> CameraState {
    let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let (guard, _) = STATE_CHANGED
        .wait_timeout_while(guard, timeout, |state| *state == seen)
        .unwrap_or_else(|e| e.into_inner());
    *guard
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

/// Records when the start command for the running start arrived (`None` at stop).
pub fn set_started_at(at: Option<Instant>) {
    *STARTED_AT.lock().unwrap_or_else(|e| e.into_inner()) = at;
}

/// The time from the start command to now, the first time it is asked after
/// a start, and `None` after that: the first frame's delay.
pub fn first_frame_since_start() -> Option<Duration> {
    STARTED_AT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .map(|t| t.elapsed())
}

/// Takes the read lock. Reads never fail; a poisoned lock is still usable here.
pub fn read_camera() -> RwLockReadGuard<'static, ()> {
    CAMERA.read().unwrap_or_else(|e| e.into_inner())
}
