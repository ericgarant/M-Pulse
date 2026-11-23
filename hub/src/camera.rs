//! Cameras on the hub's network.
//!
//! Each camera announces itself to UDP port 4220 (see `m_pulse_core::camera`)
//! and the hub keeps a registry of them. A camera is started only while a
//! viewer wants it: the hub resends `start` once a second until the camera
//! says it is streaming (15 tries at most), sends `stop` once when a wish
//! ends, and drops a wish that nobody has renewed for 60 s, so a closed
//! browser does not keep a camera on.
//!
//! Each camera also has a picture rotation (0, 90, 180 or 270 degrees), set
//! from the dashboard. It is kept by name and saved to flash (see [`Saved`]),
//! so it survives a reboot and a camera that is offline.
//!
//! The registry lives in [`crate::state::Shared`]. Both threads hold its lock
//! only long enough to update or read it; the UDP sends happen outside it.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::camera::{self, Announce, CameraState, Command, MAX_NAME_LEN};
use m_pulse_core::fuel::fnv1a;

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
/// A camera going to standby this soon after the hub's `stop` is obeying
/// that stop, not stopping on its own.
const STOP_ECHO: Duration = Duration::from_secs(10);
/// The picture rotations the dashboard may set, in degrees.
pub const ROTATIONS: [u16; 4] = [0, 90, 180, 270];

/// Parses a rotation from the dashboard (`90`). Returns it only if it is one
/// of [`ROTATIONS`].
pub fn parse_rotation(text: &str) -> Option<u16> {
    let v: u16 = text.trim().parse().ok()?;
    ROTATIONS.contains(&v).then_some(v)
}

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
    /// When the hub last sent this camera `stop`.
    stop_sent: Option<Instant>,
}

#[derive(Default)]
pub struct Cameras {
    list: Vec<Camera>,
    /// Rotation in degrees, by camera name. A name with no entry is 0.
    rotations: Vec<(String, u16)>,
}

impl Cameras {
    /// Records an announcement. The camera's address is the packet's source.
    pub fn announce(&mut self, a: Announce, ip: Ipv4Addr, now: Instant) {
        if a.state == CameraState::Streaming {
            crate::radio::video_seen();
        }
        if let Some(c) = self.list.iter_mut().find(|c| c.name == a.name) {
            c.ip = ip;
            // A camera that was streaming and is now in standby has stopped on
            // its own (nobody watched), unless the hub just told it to stop. On
            // its own, a wish still running would only start it again, so end
            // it. After the hub's stop, a wish is a new Watch and must stay.
            let hub_stopped = c.stop_sent.is_some_and(|t| now.duration_since(t) < STOP_ECHO);
            if c.state == CameraState::Streaming && a.state == CameraState::Standby && !hub_stopped {
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
            stop_sent: None,
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
                c.stop_sent = Some(now);
                out.push((addr(c.ip), Command::Stop));
            }
        }
        out
    }

    /// Whether the hub has heard from a camera with this name.
    pub fn knows(&self, name: &str) -> bool {
        self.list.iter().any(|c| c.name == name)
    }

    /// Sets a camera's picture rotation. Does nothing for a camera the hub
    /// has never heard from. Only [`ROTATIONS`] values reach here.
    pub fn set_rotation(&mut self, name: &str, degrees: u16) {
        if !self.knows(name) {
            return;
        }
        match self.rotations.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = degrees,
            None => {
                // Only MAX_CAMERAS rotations fit in flash. Make room by dropping
                // names the hub no longer hears from, then the oldest entry.
                if self.rotations.len() >= MAX_CAMERAS {
                    let list = &self.list;
                    self.rotations.retain(|(n, _)| list.iter().any(|c| &c.name == n));
                }
                while self.rotations.len() >= MAX_CAMERAS {
                    self.rotations.remove(0);
                }
                self.rotations.push((name.to_string(), degrees));
            }
        }
    }

    /// A camera's rotation in degrees (0 if none has been set).
    pub fn rotation(&self, name: &str) -> u16 {
        self.rotations
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, degrees)| *degrees)
    }

    /// The rotations as they are saved to flash.
    pub fn saved(&self) -> Saved {
        Saved {
            rotations: self.rotations.clone(),
        }
    }

    /// Takes the rotations loaded from flash at boot.
    pub fn restore(&mut self, saved: &Saved) {
        self.rotations = saved.rotations.clone();
    }

    /// The camera list for `GET /api/cameras`, as a JSON array.
    pub fn json_list(&self, now: Instant) -> String {
        let entries: Vec<String> = self
            .list
            .iter()
            .map(|c| entry_json(c, self.rotation(&c.name), now))
            .collect();
        format!("[{}]", entries.join(","))
    }

    /// One camera's entry as JSON, or `None` if the hub has never heard of it.
    pub fn json_entry(&self, name: &str, now: Instant) -> Option<String> {
        self.list
            .iter()
            .find(|c| c.name == name)
            .map(|c| entry_json(c, self.rotation(name), now))
    }
}

/// The picture rotations kept in flash, so a restart keeps them. Each camera
/// is one slot, so the record is a fixed size: a version, the count, the
/// slots (name length, name padded to [`MAX_NAME_LEN`], rotation as quarter
/// turns 0 to 3),
/// then a sequence number and a checksum. The layout follows
/// `thermostat::Saved`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Saved {
    rotations: Vec<(String, u16)>,
}

impl Saved {
    /// Version 2 stores quarter turns (0-3). Version 1 stored degrees in one
    /// byte, so 270 was written as 14; it is still read, with 14 as 270.
    const VERSION: u8 = 2;
    const SLOT: usize = 1 + MAX_NAME_LEN + 1;
    /// Bytes in [`Saved::encode`]'s output.
    pub const LEN: usize = 2 + MAX_CAMERAS * Self::SLOT + 8;

    pub fn encode(&self, seq: u32) -> Vec<u8> {
        let mut out = vec![0u8; Self::LEN];
        out[0] = Self::VERSION;
        // The hub keeps one rotation per announced camera, at most MAX_CAMERAS.
        out[1] = self.rotations.len().min(MAX_CAMERAS) as u8;
        for (i, (name, degrees)) in self.rotations.iter().take(MAX_CAMERAS).enumerate() {
            let at = 2 + i * Self::SLOT;
            out[at] = name.len() as u8;
            out[at + 1..at + 1 + name.len()].copy_from_slice(name.as_bytes());
            out[at + Self::SLOT - 1] = (*degrees / 90) as u8;
        }
        let seq_at = Self::LEN - 8;
        out[seq_at..seq_at + 4].copy_from_slice(&seq.to_le_bytes());
        let sum = fnv1a(&out[..seq_at + 4]);
        out[seq_at + 4..].copy_from_slice(&sum.to_le_bytes());
        out
    }

    /// Decodes a saved copy and its `seq`. Returns `None` for anything
    /// damaged, from another version, or with a name or rotation the hub
    /// could not have saved.
    pub fn decode(bytes: &[u8]) -> Option<(Self, u32)> {
        if bytes.len() != Self::LEN || !(bytes[0] == 1 || bytes[0] == Self::VERSION) {
            return None;
        }
        let version = bytes[0];
        let seq_at = Self::LEN - 8;
        let word = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        if fnv1a(&bytes[..seq_at + 4]) != word(seq_at + 4) {
            return None;
        }
        let count = usize::from(bytes[1]);
        if count > MAX_CAMERAS {
            return None;
        }
        let mut rotations = Vec::with_capacity(count);
        for i in 0..count {
            let at = 2 + i * Self::SLOT;
            let len = usize::from(bytes[at]);
            if len > MAX_NAME_LEN {
                return None;
            }
            let name = std::str::from_utf8(&bytes[at + 1..at + 1 + len]).ok()?;
            if !camera::valid_name(name) {
                return None;
            }
            let byte = bytes[at + Self::SLOT - 1];
            let degrees = match (version, byte) {
                (Self::VERSION, 0..=3) => u16::from(byte) * 90,
                (1, 0 | 90 | 180) => u16::from(byte),
                (1, 14) => 270, // version 1 wrote 270 as one byte
                _ => return None,
            };
            rotations.push((name.to_string(), degrees));
        }
        Some((Self { rotations }, word(seq_at)))
    }
}

fn addr(ip: Ipv4Addr) -> SocketAddr {
    SocketAddr::from((ip, camera::COMMAND_PORT))
}

/// One camera as JSON. Names are checked by `Announce::parse`, so they need
/// no escaping.
fn entry_json(c: &Camera, rotation: u16, now: Instant) -> String {
    let online = now.duration_since(c.last_seen) < Duration::from_secs(camera::OFFLINE_AFTER_S);
    let wanted = c.wish.as_ref().is_some_and(|w| now < w.until);
    let gave_up = c
        .wish
        .as_ref()
        .is_some_and(|w| w.tries >= MAX_TRIES && c.state != CameraState::Streaming);
    let stream_url = if online && c.state == CameraState::Streaming {
        // The camera serves its stream on port 81, apart from its page, still and settings on 80.
        format!("\"http://{}:81/stream\"", c.ip)
    } else {
        "null".to_string()
    };
    format!(
        "{{\"name\":\"{}\",\"ip\":\"{}\",\"state\":\"{}\",\"online\":{online},\"wanted\":{wanted},\
         \"rotation\":{rotation},\"note\":{},\"stream_url\":{stream_url},\"still_url\":\"http://{}/still.jpg\"}}",
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
