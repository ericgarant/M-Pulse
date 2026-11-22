//! Camera messages between the camera nodes and the hub over UDP.
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
}
