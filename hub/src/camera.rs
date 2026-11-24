//! Cameras on the hub's network: the UDP side.
//!
//! Each camera announces itself to UDP port 4220, and the hub keeps a
//! registry of them, [`m_pulse_core::camera::Cameras`]. The registry decides
//! when to send `start` and `stop` and keeps each camera's rotation; this
//! module receives the announcements and sends what the registry says is due.
//!
//! The registry lives in [`crate::state::Shared`]. Both threads hold its lock
//! only long enough to update or read it; the UDP sends happen outside it.

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::camera::{self, Announce, SEND_EVERY};

use crate::state::Shared;

/// Starts the receiver for camera announcements and the sender for start
/// and stop commands.
pub fn spawn(state: Shared) -> anyhow::Result<()> {
    let rx_state = state.clone();
    thread::Builder::new()
        .name("camera-rx".into())
        .stack_size(6 * 1024)
        .spawn(move || loop {
            if let Err(err) = receive(&rx_state) {
                warn!("Camera receiver stopped: {err}. Restarting");
                thread::sleep(Duration::from_secs(5));
            }
        })?;
    thread::Builder::new()
        .name("camera-tx".into())
        .stack_size(6 * 1024)
        .spawn(move || send_loop(&state))?;
    Ok(())
}

fn receive(state: &Shared) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(("0.0.0.0", camera::ANNOUNCE_PORT))?;
    info!("Listening for cameras on UDP port {}", camera::ANNOUNCE_PORT);

    let mut buf = [0u8; 64];
    loop {
        let (len, from) = socket.recv_from(&mut buf)?;
        // Anything else on this port is ignored, without a log line.
        let (SocketAddr::V4(from), Some(announce)) = (from, Announce::parse(&buf[..len])) else {
            continue;
        };
        // A camera that says it is streaming holds back Bluetooth scans.
        let video = state.cameras().announce(announce, *from.ip(), Instant::now());
        if video {
            crate::radio::video_seen();
        }
    }
}

/// Set by [`send_now`] to wake the sender before its next tick.
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Wakes the sender at once, so a start or stop the dashboard just asked for
/// goes out now instead of at the next tick (up to a second later).
pub fn send_now() {
    let (woken, wake) = &WAKE;
    *woken.lock().unwrap_or_else(|e| e.into_inner()) = true;
    wake.notify_one();
}

/// Waits for the next tick, or less if [`send_now`] is called.
fn wait_for_tick() {
    let (woken, wake) = &WAKE;
    let guard = woken.lock().unwrap_or_else(|e| e.into_inner());
    let (mut guard, _) = wake
        .wait_timeout_while(guard, SEND_EVERY, |woken| !*woken)
        .unwrap_or_else(|e| e.into_inner());
    *guard = false;
}

/// Once a second, and at once when the dashboard asks, sends whatever the
/// registry says is due.
fn send_loop(state: &Shared) {
    let socket = loop {
        match UdpSocket::bind(("0.0.0.0", 0)) {
            Ok(s) => break s,
            Err(err) => {
                warn!("Camera sender: {err}. Retrying");
                thread::sleep(Duration::from_secs(5));
            }
        }
    };
    loop {
        wait_for_tick();
        let due = state.cameras().due(Instant::now());
        for (to, command) in due {
            if let Err(err) = socket.send_to(command.encode().as_bytes(), to) {
                warn!("Camera command to {to} failed: {err}");
            }
        }
    }
}
