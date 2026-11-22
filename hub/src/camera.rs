//! Cameras on the hub's network.
//!
//! Each camera announces itself to UDP port 4220 (see `m_pulse_core::camera`)
//! and the hub keeps a registry of them. A camera is started only while a
//! viewer wants it: the hub resends `start` once a second until the camera
//! says it is streaming (15 tries at most), sends `stop` once when a wish
//! ends, and drops a wish that nobody has renewed for 60 s, so a closed
//! browser does not keep a camera on.
//!
//! The registry lives in [`crate::state::Shared`]. Both threads hold its lock
//! only long enough to update or read it; the UDP sends happen outside it.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::camera::{self, Announce, CameraState, Command};

use crate::state::Shared;

/// Most cameras the registry keeps. Announcements from a new name past this
/// are ignored.
const MAX_CAMERAS: usize = 8;
/// Sends of `start` before the hub stops trying and reports no answer.
const MAX_TRIES: u8 = 15;
/// How long a viewer's request keeps a camera wanted without a renewal.
const WISH_FOR: Duration = Duration::from_secs(60);
/// Pause between `start` resends.
const RESEND_EVERY: Duration = Duration::from_secs(1);

/// A viewer's request that a camera be started.
struct Wish {
    until: Instant,
    tries: u8,
    last_sent: Option<Instant>,
}

pub struct Camera {
    pub name: String,
    pub ip: Ipv4Addr,
    pub state: CameraState,
    pub last_seen: Instant,
    wish: Option<Wish>,
    /// A `stop` is waiting to be sent (set when a wish ends).
    stop_due: bool,
}

#[derive(Default)]
pub struct Cameras {
    list: Vec<Camera>,
}

impl Cameras {
    /// Records an announcement. The camera's address is the packet's source.
    pub fn announce(&mut self, a: Announce, ip: Ipv4Addr, now: Instant) {
        if let Some(c) = self.list.iter_mut().find(|c| c.name == a.name) {
            c.ip = ip;
            // A camera that was streaming and is now in standby has stopped on
            // its own (nobody watched). A wish still running would only start
            // it again, so end the wish here.
            if c.state == CameraState::Streaming && a.state == CameraState::Standby {
                if c.wish.take().is_some() {
                    info!("Camera \"{}\" went to standby on its own: wish ended", c.name);
                }
            }
            c.state = a.state;
            c.last_seen = now;
            return;
        }
        if self.list.len() >= MAX_CAMERAS {
            warn!("Camera \"{}\" ignored: the hub keeps {MAX_CAMERAS} at most", a.name);
            return;
        }
        info!("Camera \"{}\" announced from {ip}", a.name);
        self.list.push(Camera {
            name: a.name,
            ip,
            state: a.state,
            last_seen: now,
            wish: None,
            stop_due: false,
        });
    }

    /// A viewer wants the camera. Starts it, or renews a wish that is already
    /// running. Returns false for a camera the hub has never heard from.
    pub fn want(&mut self, name: &str, now: Instant) -> bool {
        let Some(c) = self.list.iter_mut().find(|c| c.name == name) else {
            return false;
        };
        let streaming = c.state == CameraState::Streaming;
        // A Stop that has not gone out yet is cancelled by a new Watch, so a quick
        // Stop then Watch keeps the camera streaming.
        c.stop_due = false;
        match &mut c.wish {
            Some(w) => {
                w.until = now + WISH_FOR;
                // Asking again after a no answer starts a fresh round of tries.
                if w.tries >= MAX_TRIES && !streaming {
                    w.tries = 0;
                    w.last_sent = None;
                }
            }
            None => {
                c.wish = Some(Wish {
                    until: now + WISH_FOR,
                    tries: 0,
                    last_sent: None,
                });
            }
        }
        true
    }

    /// A viewer no longer wants the camera. Sends one `stop`. Returns false
    /// for a camera the hub has never heard from.
    pub fn unwant(&mut self, name: &str) -> bool {
        let Some(c) = self.list.iter_mut().find(|c| c.name == name) else {
            return false;
        };
        c.wish = None;
        c.stop_due = true;
        true
    }

    /// Works out what to send at `now`: `start` resends for wanted cameras,
    /// and the one `stop` for each wish that just ended. Expires old wishes.
    pub fn due(&mut self, now: Instant) -> Vec<(SocketAddr, Command)> {
        let mut out = Vec::new();
        for c in &mut self.list {
            if c.wish.as_ref().is_some_and(|w| now >= w.until) {
                info!("Camera \"{}\": no request for a minute, letting it sleep", c.name);
                c.wish = None;
                c.stop_due = true;
            }
            let streaming = c.state == CameraState::Streaming;
            if let Some(w) = &mut c.wish {
                let due = w.last_sent.is_none_or(|t| now.duration_since(t) >= RESEND_EVERY);
                if !streaming && w.tries < MAX_TRIES && due {
                    w.tries += 1;
                    w.last_sent = Some(now);
                    out.push((addr(c.ip), Command::Start));
                    if w.tries == MAX_TRIES {
                        warn!("Camera \"{}\": no answer to {MAX_TRIES} start commands", c.name);
                    }
                }
            }
            if c.stop_due {
                c.stop_due = false;
                out.push((addr(c.ip), Command::Stop));
            }
        }
        out
    }

    /// The camera list for `GET /api/cameras`, as a JSON array.
    pub fn json_list(&self, now: Instant) -> String {
        let entries: Vec<String> = self.list.iter().map(|c| entry_json(c, now)).collect();
        format!("[{}]", entries.join(","))
    }

    /// One camera's entry as JSON, or `None` if the hub has never heard of it.
    pub fn json_entry(&self, name: &str, now: Instant) -> Option<String> {
        self.list
            .iter()
            .find(|c| c.name == name)
            .map(|c| entry_json(c, now))
    }
}

fn addr(ip: Ipv4Addr) -> SocketAddr {
    SocketAddr::from((ip, camera::COMMAND_PORT))
}

/// One camera as JSON. Names are checked by `Announce::parse`, so they need
/// no escaping.
fn entry_json(c: &Camera, now: Instant) -> String {
    let online = now.duration_since(c.last_seen) < Duration::from_secs(camera::OFFLINE_AFTER_S);
    let wanted = c.wish.as_ref().is_some_and(|w| now < w.until);
    let gave_up = c
        .wish
        .as_ref()
        .is_some_and(|w| w.tries >= MAX_TRIES && c.state != CameraState::Streaming);
    let stream_url = if online && c.state == CameraState::Streaming {
        format!("\"http://{}/stream\"", c.ip)
    } else {
        "null".to_string()
    };
    format!(
        "{{\"name\":\"{}\",\"ip\":\"{}\",\"state\":\"{}\",\"online\":{online},\"wanted\":{wanted},\
         \"note\":{},\"stream_url\":{stream_url},\"still_url\":\"http://{}/still.jpg\"}}",
        c.name,
        c.ip,
        c.state.as_str(),
        if gave_up { "\"no answer\"" } else { "null" },
        c.ip
    )
}

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
        state.cameras().announce(announce, *from.ip(), Instant::now());
    }
}

/// Once a second, sends whatever the registry says is due.
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
        thread::sleep(RESEND_EVERY);
        let due = state.cameras().due(Instant::now());
        for (to, command) in due {
            if let Err(err) = socket.send_to(command.encode().as_bytes(), to) {
                warn!("Camera command to {to} failed: {err}");
            }
        }
    }
}
