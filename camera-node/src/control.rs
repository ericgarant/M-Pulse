//! Control threads: start and stop commands from the hub, announcements to
//! the hub, and the idle timer that puts the camera back to standby.

use std::net::{Ipv4Addr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::camera::{
    Announce, CameraState, Command, ANNOUNCE_EVERY_S, ANNOUNCE_PORT, COMMAND_PORT,
};

use crate::camera;
use crate::net::Online;
use crate::state::{self, IDLE_OFF};

/// How often the idle timer checks for viewers.
const IDLE_CHECK: Duration = Duration::from_secs(5);

/// Starts the command, announce and idle threads.
pub fn spawn(name: &'static str, online: Online) -> anyhow::Result<()> {
    let commands = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, COMMAND_PORT))?;
    thread::Builder::new()
        .name("commands".into())
        .stack_size(8 * 1024)
        .spawn(move || run_commands(commands))?;

    thread::Builder::new()
        .name("announce".into())
        .stack_size(6 * 1024)
        .spawn(move || run_announce(name, online))?;

    thread::Builder::new()
        .name("idle".into())
        .stack_size(4 * 1024)
        .spawn(run_idle)?;
    Ok(())
}

fn run_commands(socket: UdpSocket) {
    let mut buf = [0u8; 128];
    loop {
        let Ok((len, from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        match Command::parse(&buf[..len]) {
            Some(Command::Start) => start(),
            Some(Command::Stop) => stop(),
            None => warn!("Ignoring a command from {from}"),
        }
    }
}

fn start() {
    if state::state() != CameraState::Standby {
        return; // already starting or streaming; repeats from the hub are harmless
    }
    state::set_state(CameraState::Starting);
    info!("Start requested: powering up the camera");
    let result = {
        let _write = state::CAMERA.write().unwrap_or_else(|e| e.into_inner());
        camera::power_up()
    };
    match result {
        Ok(()) => {
            state::touch_viewed();
            crate::net::set_power_save(false);
            state::set_state(CameraState::Streaming);
            info!("Camera streaming");
        }
        Err(err) => {
            warn!("Could not start the camera: {err}");
            state::set_state(CameraState::Standby);
        }
    }
}

fn stop() {
    if state::state() == CameraState::Standby {
        return;
    }
    let _write = state::CAMERA.write().unwrap_or_else(|e| e.into_inner());
    camera::power_down();
    state::set_state(CameraState::Standby);
    crate::net::set_power_save(true);
    info!("Camera in standby");
}

/// Sends an announcement whenever the state changes, and every few seconds otherwise.
fn run_announce(name: &'static str, online: Online) {
    let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        Ok(s) => s,
        Err(err) => {
            warn!("Announce socket failed: {err}");
            return;
        }
    };
    if let Err(err) = socket.set_broadcast(true) {
        warn!("Announce broadcast setup failed: {err}");
        return;
    }
    let target = (Ipv4Addr::BROADCAST, ANNOUNCE_PORT);
    let mut last_sent: Option<(CameraState, Instant)> = None;
    loop {
        thread::sleep(Duration::from_secs(1));
        if !online.is_up() {
            continue;
        }
        let now_state = state::state();
        let due = match last_sent {
            None => true,
            Some((sent_state, at)) => {
                sent_state != now_state || at.elapsed() >= Duration::from_secs(ANNOUNCE_EVERY_S)
            }
        };
        if !due {
            continue;
        }
        let packet = Announce {
            name: name.to_string(),
            state: now_state,
        };
        if let Err(err) = socket.send_to(packet.encode().as_bytes(), target) {
            warn!("Announce failed: {err}");
        }
        last_sent = Some((now_state, Instant::now()));
    }
}

/// Backup: puts a streaming camera back to standby after nobody has watched for
/// `IDLE_OFF`. The hub's stop command is the normal way to sleep.
fn run_idle() {
    loop {
        thread::sleep(IDLE_CHECK);
        if state::state() != CameraState::Streaming {
            continue;
        }
        if state::VIEWERS.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            state::touch_viewed();
            continue;
        }
        if state::idle_for() >= IDLE_OFF {
            info!("No viewers for {} s: camera to standby", IDLE_OFF.as_secs());
            stop();
        }
    }
}
