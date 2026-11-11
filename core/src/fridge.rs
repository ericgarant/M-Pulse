//! Wire format of the BougeRV Rocky 50 fridge's BLE protocol, and the
//! warm-fridge alert.
//!
//! Taken from the vendor's Android app (v2.6.2, `DetailIceRockyActivity`)
//! and checked against a live fridge (`SYZ-R50-...`). It is the protocol
//! used by Alpicool-family compressor fridges.
//!
//! Every frame is `FE FE len cmd payload.. sum_hi sum_lo`. `len` counts the
//! bytes after itself (command, payload and the two checksum bytes), and the
//! checksum is the 16-bit sum of every byte before it.
//!
//! The fridge never reports on its own: it answers each status query with
//! one status frame. The app asks every couple of seconds. It also answers a
//! settings write with a status frame (command `02` instead of `01`) and
//! echoes a target change back unchanged.

/// Ask for the status frame.
pub const CMD_QUERY: u8 = 0x01;
/// Write the whole settings block (power, mode, lock, battery protection...).
pub const CMD_SETTINGS: u8 = 0x02;
/// Set the (left, and on a single-zone fridge the only) zone's target.
pub const CMD_SET_LEFT: u8 = 0x05;

const HEADER: [u8; 2] = [0xFE, 0xFE];
/// A full status frame: header, len, command, 30 data bytes, checksum.
pub const STATUS_LEN: usize = 36;

/// Builds a frame around `command` and `payload`.
pub fn frame(command: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 6);
    out.extend_from_slice(&HEADER);
    // command + payload + two checksum bytes
    out.push((payload.len() + 3) as u8);
    out.push(command);
    out.extend_from_slice(payload);
    let sum = checksum(&out);
    out.extend_from_slice(&sum.to_be_bytes());
    out
}

/// 16-bit sum of `bytes`.
pub fn checksum(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0u16, |s, b| s.wrapping_add(u16::from(*b)))
}

/// The status query, `FE FE 03 01 02 00`.
pub fn query_frame() -> Vec<u8> {
    frame(CMD_QUERY, &[])
}

/// Sets the target temperature, in the fridge's own unit.
pub fn set_temp_frame(target: i8) -> Vec<u8> {
    frame(CMD_SET_LEFT, &[target as u8])
}

/// Run mode. The app labels them Eco and Max.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    Max,
    Eco,
}

/// Battery protection cut-off level. The app labels them L, M and H.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryProtection {
    Low,
    Medium,
    High,
}

/// A decoded status frame.
///
/// `raw` keeps the frame, because a settings write must send back every
/// setting the hub does not mean to change exactly as the fridge reported it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    pub locked: bool,
    pub powered: bool,
    pub mode: RunMode,
    pub battery_protection: BatteryProtection,
    /// Target temperature.
    pub set_temp: i8,
    /// Lowest and highest target the fridge accepts.
    pub min_temp: i8,
    pub max_temp: i8,
    /// Temperature inside.
    pub temp: i8,
    /// Whether temperatures are in Fahrenheit (otherwise Celsius).
    pub fahrenheit: bool,
    /// The fridge's battery gauge, 0 to 100. Reads 100 on DC input.
    pub battery_pct: u8,
    /// Supply voltage at the fridge's input.
    pub input_volts: f32,
    raw: [u8; STATUS_LEN],
}

impl Status {
    /// Parses a status frame. Returns `None` if it is not one, has the wrong
    /// length, or fails its checksum.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() != STATUS_LEN
            || data[..2] != HEADER
            || usize::from(data[2]) != STATUS_LEN - 3
            || !matches!(data[3], CMD_QUERY | CMD_SETTINGS)
        {
            return None;
        }
        let sum = u16::from_be_bytes([data[STATUS_LEN - 2], data[STATUS_LEN - 1]]);
        if checksum(&data[..STATUS_LEN - 2]) != sum {
            return None;
        }
        let mut raw = [0u8; STATUS_LEN];
        raw.copy_from_slice(data);
        let i = |n: usize| data[n] as i8;
        Some(Self {
            locked: data[4] != 0,
            powered: data[5] != 0,
            mode: if data[6] == 1 {
                RunMode::Eco
            } else {
                RunMode::Max
            },
            battery_protection: match data[7] {
                0 => BatteryProtection::Low,
                1 => BatteryProtection::Medium,
                _ => BatteryProtection::High,
            },
            set_temp: i(8),
            max_temp: i(9),
            min_temp: i(10),
            fahrenheit: data[13] != 0,
            temp: i(18),
            battery_pct: data[19],
            input_volts: f32::from(data[20]) + f32::from(data[21]) * 0.1,
            raw,
        })
    }

    /// The settings block with power switched on or off and nothing else
    /// changed.
    pub fn power_frame(&self, on: bool) -> Vec<u8> {
        let mut s = self.settings();
        s[1] = u8::from(on);
        frame(CMD_SETTINGS, &s)
    }

    /// The settings block with the run mode changed and nothing else.
    pub fn mode_frame(&self, mode: RunMode) -> Vec<u8> {
        let mut s = self.settings();
        s[2] = u8::from(mode == RunMode::Eco);
        frame(CMD_SETTINGS, &s)
    }

    /// The 25-byte settings block as the app builds it from the last status:
    /// lock .. left downtime (status bytes 4 to 17), right target .. right
    /// downtime (22 to 29), then hot target (33), fridge model (32) and a
    /// trailing zero. The current temperatures, battery gauge and voltage are
    /// readings, not settings, so they are left out.
    fn settings(&self) -> [u8; 25] {
        let r = &self.raw;
        let mut s = [0u8; 25];
        s[..14].copy_from_slice(&r[4..18]);
        s[14..22].copy_from_slice(&r[22..30]);
        s[22] = r[33];
        s[23] = r[32];
        s[24] = 0;
        s
    }
}

/// Collects notification bytes until a whole status frame is in, in case
/// the link splits a frame across notifications.
#[derive(Default)]
pub struct Reassembler {
    buf: Vec<u8>,
}

impl Reassembler {
    /// Adds bytes and returns a frame once one is complete.
    pub fn push(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        self.buf.extend_from_slice(data);
        // Drop anything before a frame header.
        while self.buf.len() >= 2 && self.buf[..2] != HEADER {
            self.buf.remove(0);
        }
        if self.buf.len() < 3 {
            return None;
        }
        let total = usize::from(self.buf[2]) + 3;
        if self.buf.len() < total {
            // Guard against a garbage length leaving the buffer to grow.
            if self.buf.len() > 2 * STATUS_LEN {
                self.buf.clear();
            }
            return None;
        }
        let out: Vec<u8> = self.buf.drain(..total).collect();
        Some(out)
    }
}

/// How far above its target the fridge may be before it counts as warm.
pub const WARM_MARGIN: i8 = 4;
/// How long it must stay warm before the alert shows, in seconds.
pub const WARM_AFTER_S: u64 = 30 * 60;

/// Watches for a fridge that stays too warm: lid left open, low supply, or a
/// fault. Times are seconds on any steady clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct WarmWatch {
    warm_since: Option<u64>,
}

impl WarmWatch {
    /// Feeds one status and returns whether the alert is on.
    ///
    /// A fridge that is switched off is not watched: it is meant to be warm.
    pub fn update(&mut self, now_s: u64, status: &Status) -> bool {
        let warm = status.powered && status.temp > status.set_temp.saturating_add(WARM_MARGIN);
        if !warm {
            self.warm_since = None;
            return false;
        }
        let since = *self.warm_since.get_or_insert(now_s);
        now_s.saturating_sub(since) >= WARM_AFTER_S
    }

    /// Seconds the fridge has been warm, if it is.
    pub fn warm_for(&self, now_s: u64) -> Option<u64> {
        self.warm_since.map(|s| now_s.saturating_sub(s))
    }

    /// Forgets the warm spell, for when readings stop.
    pub fn reset(&mut self) {
        self.warm_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A status frame captured from a real Rocky 50: on, Max, unlocked,
    /// battery protection H, target 4 °C, 9 °C inside, 14.4 V input.
    const LIVE: [u8; 36] = [
        0xfe, 0xfe, 0x21, 0x01, 0x00, 0x01, 0x00, 0x02, 0x04, 0x14, 0xec, 0x02, 0x00, 0x00, 0xfd,
        0xfd, 0xff, 0x00, 0x09, 0x64, 0x0e, 0x04, 0x04, 0x00, 0x00, 0x02, 0xfd, 0xfd, 0x00, 0x00,
        0x09, 0x01, 0x00, 0x00, 0x08, 0xa9,
    ];

    #[test]
    fn query_matches_the_vendor_app() {
        assert_eq!(query_frame(), [0xfe, 0xfe, 0x03, 0x01, 0x02, 0x00]);
    }

    #[test]
    fn set_temp_frame_layout() {
        // FE FE 04 05 03, sum 0x0208.
        assert_eq!(
            set_temp_frame(3),
            [0xfe, 0xfe, 0x04, 0x05, 0x03, 0x02, 0x08]
        );
        // Negative targets go out as two's complement.
        assert_eq!(set_temp_frame(-2)[4], 0xfe);
    }

    #[test]
    fn parses_the_live_frame() {
        let s = Status::parse(&LIVE).expect("valid frame");
        assert!(s.powered && !s.locked && !s.fahrenheit);
        assert_eq!(s.mode, RunMode::Max);
        assert_eq!(s.battery_protection, BatteryProtection::High);
        assert_eq!((s.set_temp, s.temp), (4, 9));
        assert_eq!((s.min_temp, s.max_temp), (-20, 20));
        assert_eq!(s.battery_pct, 100);
        assert!((s.input_volts - 14.4).abs() < 0.01);
    }

    #[test]
    fn parses_the_reply_to_a_settings_write() {
        // Captured after switching to Eco: same layout, command 02.
        let reply = [
            0xfe, 0xfe, 0x21, 0x02, 0x00, 0x01, 0x01, 0x02, 0x04, 0x14, 0xec, 0x02, 0x00, 0x00,
            0xfd, 0xfd, 0xff, 0x00, 0xfe, 0x64, 0x0e, 0x06, 0x04, 0x00, 0x00, 0x02, 0xfd, 0xfd,
            0x00, 0x00, 0xfe, 0x01, 0x00, 0x00, 0x0a, 0x97,
        ];
        let s = Status::parse(&reply).expect("valid frame");
        assert_eq!(s.mode, RunMode::Eco);
        assert_eq!(s.temp, -2);
    }

    #[test]
    fn rejects_corrupt_frames() {
        let mut bad = LIVE;
        bad[18] ^= 1;
        assert_eq!(Status::parse(&bad), None);
        assert_eq!(Status::parse(&LIVE[..35]), None);
    }

    #[test]
    fn settings_writes_change_one_field_only() {
        let s = Status::parse(&LIVE).unwrap();
        // The app's settings frame: 28 after the length byte, 31 in all.
        let off = s.power_frame(false);
        assert_eq!(off.len(), 31);
        assert_eq!(&off[..4], &[0xfe, 0xfe, 0x1c, 0x02]);
        let expected_payload = [
            0x00, 0x00, 0x00, 0x02, 0x04, 0x14, 0xec, 0x02, 0x00, 0x00, 0xfd, 0xfd, 0xff,
            0x00, // left
            0x04, 0x00, 0x00, 0x02, 0xfd, 0xfd, 0x00, 0x00, // right
            0x00, 0x00, 0x00, // hot target, model, trailing zero
        ];
        assert_eq!(&off[4..29], &expected_payload);
        assert_eq!(u16::from_be_bytes([off[29], off[30]]), checksum(&off[..29]));

        let on = s.power_frame(true);
        assert_eq!(on[5], 1);
        assert_eq!(on[4..].len(), off[4..].len());

        let eco = s.mode_frame(RunMode::Eco);
        assert_eq!((eco[5], eco[6]), (1, 1));
    }

    #[test]
    fn reassembles_split_frames() {
        let mut r = Reassembler::default();
        assert_eq!(r.push(&[0x00, 0x11]), None, "junk is skipped");
        assert_eq!(r.push(&LIVE[..20]), None);
        assert_eq!(r.push(&LIVE[20..]).as_deref(), Some(&LIVE[..]));
        assert_eq!(r.push(&LIVE).as_deref(), Some(&LIVE[..]));
    }

    #[test]
    fn warm_alert_needs_thirty_minutes() {
        let mut s = Status::parse(&LIVE).unwrap();
        let mut w = WarmWatch::default();
        // 9 °C against a 4 °C target is 5 over, past the 4 °C margin.
        assert!(!w.update(0, &s));
        assert!(!w.update(WARM_AFTER_S - 1, &s));
        assert!(w.update(WARM_AFTER_S, &s));
        // Back within the margin clears it at once.
        s.temp = 8;
        assert!(!w.update(WARM_AFTER_S + 1, &s));
        assert_eq!(w.warm_for(WARM_AFTER_S + 1), None);
        // A fridge switched off is never "warm".
        s.temp = 20;
        s.powered = false;
        assert!(!w.update(0, &s));
        assert!(!w.update(10 * WARM_AFTER_S, &s));
    }
}
