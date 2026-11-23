//! Camera messages between the camera nodes and the hub over UDP, and the
//! hub's registry of cameras.
//!
//! Each camera announces itself to the hub, and the hub sends it start and
//! stop commands. One short text line per datagram, as in [`crate::wire`]:
//!
//! ```text
//! MPC1 announce <name> <state>   camera -> hub, broadcast to ANNOUNCE_PORT
//! MPC1 start                     hub -> camera, unicast to COMMAND_PORT
//! MPC1 stop                      hub -> camera, unicast to COMMAND_PORT
//! ```
//!
//! `name` is a short label such as `cab` or `interior`. `state` is one of:
//!
//! - `standby`: the camera sensor is powered down and waiting for a start command
//! - `starting`: the sensor is powering up
//! - `streaming`: frames are being served at `/stream` and `/still.jpg`
//!
//! The hub takes a camera's IP address from the packet's source address.
//! A camera that has not announced for [`OFFLINE_AFTER_S`] seconds is offline.
//!
//! The hub's side of it, [`Cameras`], decides when to send `start` and `stop`
//! and keeps each camera's picture rotation ([`Saved`] in flash). It needs no
//! hardware, so it is tested here; the hub's `camera` module does the UDP.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use log::{info, warn};

use crate::fuel::fnv1a;

/// UDP port the hub listens on for camera announcements.
pub const ANNOUNCE_PORT: u16 = 4220;
/// UDP port each camera listens on for start and stop commands.
pub const COMMAND_PORT: u16 = 4221;
/// Seconds between a camera's announcements.
pub const ANNOUNCE_EVERY_S: u64 = 5;
/// Seconds without an announcement before the hub marks a camera offline.
pub const OFFLINE_AFTER_S: u64 = 15;
/// Longest name a camera may announce.
pub const MAX_NAME_LEN: usize = 16;

const TAG: &str = "MPC1";

/// What a camera is doing, as it reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraState {
    Standby,
    Starting,
    Streaming,
}

impl CameraState {
    pub fn as_str(self) -> &'static str {
        match self {
            CameraState::Standby => "standby",
            CameraState::Starting => "starting",
            CameraState::Streaming => "streaming",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "standby" => Some(CameraState::Standby),
            "starting" => Some(CameraState::Starting),
            "streaming" => Some(CameraState::Streaming),
            _ => None,
        }
    }
}

/// A camera's announcement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announce {
    pub name: String,
    pub state: CameraState,
}

impl Announce {
    pub fn encode(&self) -> String {
        format!("{TAG} announce {} {}", self.name, self.state.as_str())
    }

    /// Parses an announcement. Returns `None` for anything malformed, including
    /// names that are empty, too long or contain anything but `a-z`, `0-9` and `-`.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let text = core::str::from_utf8(data).ok()?;
        let mut fields = text.trim().split(' ');
        if fields.next()? != TAG || fields.next()? != "announce" {
            return None;
        }
        let name = fields.next()?;
        let state = CameraState::parse(fields.next()?)?;
        if fields.next().is_some() || !valid_name(name) {
            return None;
        }
        Some(Self {
            name: name.to_string(),
            state,
        })
    }
}

/// A command from the hub to a camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
}

impl Command {
    pub fn encode(self) -> &'static str {
        match self {
            Command::Start => "MPC1 start",
            Command::Stop => "MPC1 stop",
        }
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        let text = core::str::from_utf8(data).ok()?;
        match text.trim() {
            "MPC1 start" => Some(Command::Start),
            "MPC1 stop" => Some(Command::Stop),
            _ => None,
        }
    }
}

/// Camera names are short, lower-case, and safe to use in a URL.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

// ---------------------------------------------------------------------------
// The hub's registry
// ---------------------------------------------------------------------------
//
// A camera is started only while a viewer wants it: the hub resends `start`
// every 2 s until the camera says it is streaming (15 tries at most),
// sends `stop` once when a wish ends, and drops a wish that nobody has
// renewed for 60 s, so a closed browser does not keep a camera on.
//
// Each camera also has a picture rotation (0, 90, 180 or 270 degrees), set
// from the dashboard. It is kept by name and saved to flash (see [`Saved`]),
// so it survives a reboot and a camera that is offline.

/// Most cameras the registry keeps. Announcements from a new name past this
/// are ignored.
const MAX_CAMERAS: usize = 8;
/// Sends of `start` before the hub stops trying and reports no answer.
const MAX_TRIES: u8 = 15;
/// How long a viewer's request keeps a camera wanted without a renewal.
const WISH_FOR: Duration = Duration::from_secs(60);
/// How often the hub works out what to send.
pub const SEND_EVERY: Duration = Duration::from_secs(1);
/// Pause between `start` resends. A camera takes a start only 2 s after it
/// last started or stopped (`COMMAND_GAP` in camera-node/src/control.rs), so
/// at this pace each resend reaches its driver and counts as a real try.
const START_AGAIN: Duration = Duration::from_secs(2);
/// A camera going to standby this soon after the hub's `stop` is obeying
/// that stop, not stopping on its own.
const STOP_ECHO: Duration = Duration::from_secs(10);
/// A camera nobody wants that still says it is starting or streaming is sent
/// `stop` again: every STOP_AGAIN for STOP_AGAIN_TRIES times, then every
/// STOP_AGAIN_SLOW. The first stop may have been lost, or the hub restarted
/// while the camera streamed.
const STOP_AGAIN: Duration = Duration::from_secs(3);
const STOP_AGAIN_TRIES: u8 = 10;
const STOP_AGAIN_SLOW: Duration = Duration::from_secs(30);
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
    /// Stops sent again since the camera last said it was in standby or was
    /// last wanted.
    stops_again: u8,
}

#[derive(Default)]
pub struct Cameras {
    list: Vec<Camera>,
    /// Rotation in degrees, by camera name. A name with no entry is 0.
    rotations: Vec<(String, u16)>,
}

impl Cameras {
    /// Records an announcement. The camera's address is the packet's source.
    /// Returns true if the camera says it is streaming ("video seen"), which
    /// is the hub's cue to hold back its Bluetooth scans.
    pub fn announce(&mut self, a: Announce, ip: Ipv4Addr, now: Instant) -> bool {
        let video = a.state == CameraState::Streaming;
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
            if a.state == CameraState::Standby {
                c.stops_again = 0;
            }
            c.state = a.state;
            c.last_seen = now;
            return video;
        }
        if self.list.len() >= MAX_CAMERAS {
            warn!("Camera \"{}\" ignored: the hub keeps {MAX_CAMERAS} at most", a.name);
            return video;
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
            stops_again: 0,
        });
        video
    }

    /// A viewer wants the camera. Starts it, or renews a wish that is already
    /// running. `watch` is the dashboard's Watch button: after a no answer
    /// (15 starts unanswered), only it starts a fresh round of tries. Renewals
    /// and the page's own wake-ups pass false, so a camera that does not answer
    /// is not retried forever. Returns false for a camera the hub has never
    /// heard from.
    pub fn want(&mut self, name: &str, now: Instant, watch: bool) -> bool {
        let Some(c) = self.list.iter_mut().find(|c| c.name == name) else {
            return false;
        };
        let streaming = c.state == CameraState::Streaming;
        // A Stop that has not gone out yet is cancelled by a new Watch, so a quick
        // Stop then Watch keeps the camera streaming.
        c.stop_due = false;
        c.stops_again = 0;
        match &mut c.wish {
            Some(w) => {
                w.until = now + WISH_FOR;
                // Watch after a no answer starts a fresh round of tries.
                if watch && w.tries >= MAX_TRIES && !streaming {
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
    /// the `stop` for each wish that just ended, and `stop` again for a
    /// camera nobody wants that still says it is starting or streaming.
    /// Expires old wishes.
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
                let due = w.last_sent.is_none_or(|t| now.duration_since(t) >= START_AGAIN);
                if !streaming && w.tries < MAX_TRIES && due {
                    w.tries += 1;
                    w.last_sent = Some(now);
                    out.push((addr(c.ip), Command::Start));
                    if w.tries == MAX_TRIES {
                        warn!("Camera \"{}\": no answer to {MAX_TRIES} start commands", c.name);
                    }
                }
            }
            // Nobody wants it, yet it says it is on: the stop was lost, or the
            // hub restarted while it streamed. Only while it announces.
            let online = now.duration_since(c.last_seen) < Duration::from_secs(OFFLINE_AFTER_S);
            if c.wish.is_none() && c.state != CameraState::Standby && online && !c.stop_due {
                let every = if c.stops_again < STOP_AGAIN_TRIES { STOP_AGAIN } else { STOP_AGAIN_SLOW };
                if c.stop_sent.is_none_or(|t| now.duration_since(t) >= every) {
                    c.stops_again = c.stops_again.saturating_add(1);
                    if c.stops_again == 1 {
                        info!("Camera \"{}\" is {} with nobody watching: sending stop", c.name, c.state.as_str());
                    } else if c.stops_again == STOP_AGAIN_TRIES {
                        warn!("Camera \"{}\" still {}: sending stop every 30 s", c.name, c.state.as_str());
                    }
                    c.stop_due = true;
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
            if !valid_name(name) {
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
    SocketAddr::from((ip, COMMAND_PORT))
}

/// One camera as JSON. Names are checked by `Announce::parse`, so they need
/// no escaping.
fn entry_json(c: &Camera, rotation: u16, now: Instant) -> String {
    let online = now.duration_since(c.last_seen) < Duration::from_secs(OFFLINE_AFTER_S);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_round_trips() {
        let a = Announce {
            name: "cab".into(),
            state: CameraState::Standby,
        };
        assert_eq!(a.encode(), "MPC1 announce cab standby");
        assert_eq!(Announce::parse(a.encode().as_bytes()), Some(a));
    }

    #[test]
    fn announce_rejects_bad_input() {
        assert_eq!(Announce::parse(b"MPC1 announce cab sleeping"), None);
        assert_eq!(Announce::parse(b"MPC1 announce Cab standby"), None);
        assert_eq!(Announce::parse(b"MPC1 announce  standby"), None);
        assert_eq!(Announce::parse(b"MPC1 announce cab standby extra"), None);
        assert_eq!(Announce::parse(b"MP1 42 215 380"), None);
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert_eq!(
            Announce::parse(format!("MPC1 announce {long} ready").as_bytes()),
            None
        );
    }

    #[test]
    fn commands_round_trip() {
        assert_eq!(Command::parse(Command::Start.encode().as_bytes()), Some(Command::Start));
        assert_eq!(Command::parse(b"MPC1 stop\n"), Some(Command::Stop));
        assert_eq!(Command::parse(b"MPC1 reboot"), None);
    }

    // The hub's registry. Times are offsets from `t0`, so nothing waits.

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn heard(name: &str, state: CameraState) -> Announce {
        Announce {
            name: name.into(),
            state,
        }
    }

    /// A registry that has heard `cab` at 192.168.71.2 in `state` at `t0`.
    fn with_cab(state: CameraState, t0: Instant) -> Cameras {
        let mut cams = Cameras::default();
        cams.announce(heard("cab", state), Ipv4Addr::new(192, 168, 71, 2), t0);
        cams
    }

    fn sent(out: Vec<(SocketAddr, Command)>) -> Vec<Command> {
        out.into_iter().map(|(_, c)| c).collect()
    }

    fn entry(cams: &Cameras, now: Instant) -> String {
        cams.json_entry("cab", now).expect("cab is known")
    }

    #[test]
    fn announce_reports_video() {
        let t0 = Instant::now();
        let mut cams = Cameras::default();
        let ip = Ipv4Addr::new(192, 168, 71, 2);
        assert!(!cams.announce(heard("cab", CameraState::Standby), ip, t0));
        assert!(cams.announce(heard("cab", CameraState::Streaming), ip, t0));
        assert!(cams.knows("cab"));
        assert!(!cams.want("camper", t0, true), "a camera never heard from");
    }

    #[test]
    fn start_is_resent_until_streaming() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Standby, t0);
        cams.want("cab", t0, true);
        assert_eq!(sent(cams.due(t0)), [Command::Start], "the first start goes out at once");
        assert!(cams.due(t0 + secs(1)).is_empty(), "not again within 2 s");
        assert_eq!(sent(cams.due(t0 + secs(2))), [Command::Start]);
        // The hub works out what to send every second, but resends start only
        // every 2 s, the camera's own gap between starts, so each one counts.
        let at: Vec<u64> = (3..=10).filter(|&s| !cams.due(t0 + secs(s)).is_empty()).collect();
        assert_eq!(at, [4, 6, 8, 10]);
        assert!(START_AGAIN >= Duration::from_secs(2), "COMMAND_GAP in camera-node/src/control.rs");
        assert_eq!(SEND_EVERY, Duration::from_secs(1));

        cams.announce(heard("cab", CameraState::Streaming), Ipv4Addr::new(192, 168, 71, 2), t0 + secs(11));
        assert!(cams.due(t0 + secs(12)).is_empty());
        assert!(entry(&cams, t0 + secs(12)).contains("\"stream_url\":\"http://192.168.71.2:81/stream\""));
    }

    #[test]
    fn stop_then_watch_keeps_the_new_wish() {
        let t0 = Instant::now();
        let ip = Ipv4Addr::new(192, 168, 71, 2);
        let mut cams = with_cab(CameraState::Streaming, t0);
        cams.want("cab", t0, true);
        assert!(cams.due(t0).is_empty(), "already streaming");

        cams.unwant("cab");
        assert_eq!(sent(cams.due(t0 + secs(1))), [Command::Stop]);
        cams.want("cab", t0 + secs(2), true); // Watch again straight away
        // The camera obeys the stop. That is not stopping on its own, so the
        // new wish stays and starts it again.
        cams.announce(heard("cab", CameraState::Standby), ip, t0 + secs(3));
        assert!(entry(&cams, t0 + secs(3)).contains("\"wanted\":true"));
        assert_eq!(sent(cams.due(t0 + secs(3))), [Command::Start]);
    }

    #[test]
    fn watch_before_the_stop_goes_out_cancels_it() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Streaming, t0);
        cams.want("cab", t0, true);
        cams.unwant("cab");
        cams.want("cab", t0, true);
        assert!(cams.due(t0 + secs(1)).is_empty(), "no stop, and still streaming");
    }

    #[test]
    fn standby_on_its_own_ends_the_wish() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Streaming, t0);
        cams.want("cab", t0, true);
        // No stop from the hub: the camera's own idle timer put it to sleep.
        cams.announce(heard("cab", CameraState::Standby), Ipv4Addr::new(192, 168, 71, 2), t0 + secs(5));
        assert!(entry(&cams, t0 + secs(5)).contains("\"wanted\":false"));
        assert!(cams.due(t0 + secs(5)).is_empty(), "no start to wake it again");
    }

    #[test]
    fn wish_expires_without_renewal() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Streaming, t0);
        cams.want("cab", t0, true);
        assert!(cams.due(t0 + secs(59)).is_empty());
        assert!(entry(&cams, t0 + secs(59)).contains("\"wanted\":true"));
        assert_eq!(sent(cams.due(t0 + WISH_FOR)), [Command::Stop]);
        assert!(entry(&cams, t0 + WISH_FOR).contains("\"wanted\":false"));

        // A renewal before the minute is up keeps it.
        let mut cams = with_cab(CameraState::Streaming, t0);
        cams.want("cab", t0, true);
        cams.want("cab", t0 + secs(40), false);
        assert!(cams.due(t0 + secs(70)).is_empty());
    }

    #[test]
    fn no_answer_waits_for_watch() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Standby, t0);
        cams.want("cab", t0, true);
        // 15 starts, 2 s apart: the last at 28 s.
        let starts: usize = (0..40).map(|s| sent(cams.due(t0 + secs(s))).len()).sum();
        assert_eq!(starts, usize::from(MAX_TRIES));
        let later = t0 + secs(40);
        assert!(entry(&cams, later).contains("\"note\":\"no answer\""));

        // Renewals and the page's wake-ups keep the wish but do not try again.
        cams.want("cab", later, false);
        assert!(cams.due(later).is_empty());
        assert!(cams.due(later + secs(5)).is_empty());
        assert!(entry(&cams, later + secs(5)).contains("\"note\":\"no answer\""));

        // The Watch button does.
        let watch = later + secs(6);
        cams.want("cab", watch, true);
        assert!(entry(&cams, watch).contains("\"note\":null"));
        assert_eq!(sent(cams.due(watch)), [Command::Start]);
    }

    #[test]
    fn watch_does_not_restart_tries_that_are_still_running() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Standby, t0);
        cams.want("cab", t0, true);
        let first: usize = (0..5).map(|s| sent(cams.due(t0 + secs(s))).len()).sum();
        assert_eq!(first, 3, "at 0, 2 and 4 s");
        // A second Watch while the first round is under way: 12 tries left.
        cams.want("cab", t0 + secs(5), true);
        let rest: usize = (5..60).map(|s| sent(cams.due(t0 + secs(s))).len()).sum();
        assert_eq!(rest, usize::from(MAX_TRIES) - first);
    }

    /// The seconds, from 1 to `until`, at which the hub sends `stop` while the
    /// camera keeps announcing `state` once a second.
    fn stops_while(cams: &mut Cameras, state: CameraState, t0: Instant, until: u64) -> Vec<u64> {
        let ip = Ipv4Addr::new(192, 168, 71, 2);
        let mut at = Vec::new();
        for s in 1..=until {
            cams.announce(heard("cab", state), ip, t0 + secs(s));
            if sent(cams.due(t0 + secs(s))).contains(&Command::Stop) {
                at.push(s);
            }
        }
        at
    }

    #[test]
    fn lost_stop_is_sent_again() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Streaming, t0);
        cams.want("cab", t0, true);
        cams.unwant("cab");
        assert_eq!(sent(cams.due(t0)), [Command::Stop]);
        // The camera keeps streaming: every 3 s for 10 stops, then every 30 s.
        let expected: Vec<u64> = (1..=10).map(|n| n * 3).chain([60, 90]).collect();
        assert_eq!(stops_while(&mut cams, CameraState::Streaming, t0, 90), expected);
    }

    #[test]
    fn hub_restart_stops_a_camera_nobody_wants() {
        let t0 = Instant::now();
        // A fresh registry, as after a hub restart, hears a streaming camera.
        let mut cams = with_cab(CameraState::Streaming, t0);
        assert_eq!(sent(cams.due(t0)), [Command::Stop]);
        // Starting counts too.
        let mut cams = with_cab(CameraState::Starting, t0);
        assert_eq!(sent(cams.due(t0)), [Command::Stop]);
    }

    #[test]
    fn no_stop_while_wanted_in_standby_or_offline() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Streaming, t0);
        cams.want("cab", t0, true);
        cams.want("cab", t0 + secs(30), false);
        assert!(stops_while(&mut cams, CameraState::Streaming, t0, 40).is_empty(), "wanted");

        let mut cams = with_cab(CameraState::Standby, t0);
        assert!(stops_while(&mut cams, CameraState::Standby, t0, 40).is_empty(), "standby");

        // A camera that stops announcing is offline after 15 s: no more stops.
        let mut cams = with_cab(CameraState::Streaming, t0);
        assert_eq!(sent(cams.due(t0)), [Command::Stop]);
        assert_eq!(sent(cams.due(t0 + secs(3))), [Command::Stop]);
        assert!(cams.due(t0 + secs(15)).is_empty());
        assert!(cams.due(t0 + secs(18)).is_empty());
    }

    #[test]
    fn stop_resends_start_over_after_standby() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Streaming, t0);
        assert_eq!(stops_while(&mut cams, CameraState::Streaming, t0, 30).len(), 10);
        // It obeys, then later streams again unwanted: fast resends again.
        cams.announce(heard("cab", CameraState::Standby), Ipv4Addr::new(192, 168, 71, 2), t0 + secs(31));
        assert!(cams.due(t0 + secs(31)).is_empty());
        let later = t0 + secs(100);
        assert_eq!(stops_while(&mut cams, CameraState::Streaming, later, 6), [1, 4]);
    }

    #[test]
    fn rotation_makes_room_by_dropping_unheard_names() {
        let t0 = Instant::now();
        let mut cams = with_cab(CameraState::Standby, t0);
        cams.announce(heard("old0", CameraState::Standby), Ipv4Addr::new(192, 168, 71, 3), t0);
        // Flash holds MAX_CAMERAS rotations, for names mostly not heard any more.
        cams.restore(&Saved {
            rotations: (0..MAX_CAMERAS).map(|i| (format!("old{i}"), 90)).collect(),
        });
        cams.set_rotation("cab", 180);
        assert_eq!(cams.rotation("cab"), 180);
        assert_eq!(cams.rotation("old0"), 90, "a camera still heard keeps its rotation");
        assert_eq!(cams.rotation("old1"), 0, "names no longer heard are dropped");
        assert_eq!(cams.saved().rotations.len(), 2);

        cams.set_rotation("nobody", 270);
        assert_eq!(cams.rotation("nobody"), 0, "only cameras the hub has heard");
        assert_eq!(parse_rotation(" 270\n"), Some(270));
        assert_eq!(parse_rotation("45"), None);
    }

    #[test]
    fn saved_round_trips() {
        let saved = Saved {
            rotations: vec![("cab".into(), 270), ("camper".into(), 0)],
        };
        let bytes = saved.encode(42);
        assert_eq!(bytes.len(), Saved::LEN);
        assert_eq!(Saved::decode(&bytes), Some((saved, 42)));
    }

    /// A version 1 record, which stored degrees in one byte (270 as 14).
    fn v1_record(slots: &[(&str, u8)], seq: u32) -> Vec<u8> {
        let mut out = vec![0u8; Saved::LEN];
        out[0] = 1;
        out[1] = slots.len() as u8;
        for (i, (name, byte)) in slots.iter().enumerate() {
            let at = 2 + i * Saved::SLOT;
            out[at] = name.len() as u8;
            out[at + 1..at + 1 + name.len()].copy_from_slice(name.as_bytes());
            out[at + Saved::SLOT - 1] = *byte;
        }
        let seq_at = Saved::LEN - 8;
        out[seq_at..seq_at + 4].copy_from_slice(&seq.to_le_bytes());
        let sum = fnv1a(&out[..seq_at + 4]);
        out[seq_at + 4..].copy_from_slice(&sum.to_le_bytes());
        out
    }

    #[test]
    fn version_1_records_still_load() {
        let (saved, seq) = Saved::decode(&v1_record(&[("cab", 90), ("camper", 14), ("van", 0)], 7))
            .expect("a good version 1 record");
        assert_eq!(seq, 7);
        assert_eq!(
            saved.rotations,
            vec![("cab".to_string(), 90), ("camper".to_string(), 270), ("van".to_string(), 0)]
        );
        // Version 1 never wrote other bytes, and a bad checksum is rejected.
        assert_eq!(Saved::decode(&v1_record(&[("cab", 45)], 7)), None);
        let mut damaged = v1_record(&[("cab", 90)], 7);
        damaged[3] ^= 1;
        assert_eq!(Saved::decode(&damaged), None);
    }
}
