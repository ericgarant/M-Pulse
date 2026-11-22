//! Camera state shared between the control threads, the camera driver and
//! the HTTP server.
//!
//! The camera driver is only valid while it is started, so frames are read
//! under [`CAMERA`]'s read lock and starting or stopping takes the write lock.
//! The driver is never shut down while a frame is still in use.

use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

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
pub const IDLE_OFF: Duration = Duration::from_secs(30);

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
