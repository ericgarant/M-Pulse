//! Wire format of the BougeRV DHG001 diesel heater's BLE protocol.
//!
//! Taken from the vendor's Android app (v2.6.2).
//!
//! Commands are written to the "RX cmd." characteristic as
//! `[0x00, command, payload.., checksum]`, where the checksum is the low byte
//! of the sum of all preceding bytes.
//!
//! A known command that this firmware does not send:
//!
//! - `0x07`: power level, payload is kilowatts times ten (14 = 1.4 kW)

/// Ask for the firmware version. The app sends this right after connecting.
pub const CMD_VERSION: u8 = 0x0E;
/// Set the heater's clock. The app sends this once status is subscribed.
pub const CMD_CLOCK: u8 = 0x0A;

/// Switch the heater on (payload 1) or off (payload 0).
pub const CMD_SWITCH: u8 = 0x05;

/// Builds the switch command.
pub fn switch_frame(switch: crate::actuator::Switch) -> Vec<u8> {
    let on = matches!(switch, crate::actuator::Switch::On);
    frame(CMD_SWITCH, &[u8::from(on)])
}

/// Builds the clock sync command: `YY MM DD hh mm ss`, two-digit year.
pub fn clock_frame(t: &crate::clock::LocalTime) -> Vec<u8> {
    frame(
        CMD_CLOCK,
        &[
            (t.year % 100) as u8,
            t.month,
            t.day,
            t.hour,
            t.minute,
            t.second,
        ],
    )
}

/// Packet type of a status report on the "TX dat." characteristic.
const TYPE_STATUS: u8 = 0x41;
const STATUS_LEN: usize = 20;

/// Low byte of the sum of `bytes`.
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, b| sum.wrapping_add(*b))
}

/// Builds a command frame.
pub fn frame(command: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 3);
    out.push(0x00);
    out.push(command);
    out.extend_from_slice(payload);
    out.push(checksum(&out));
    out
}

/// What the heater is doing, from the low two bits of the run byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Off,
    On,
    ShuttingDown,
    Starting,
}

/// A decoded status report. The heater sends one per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    pub state: RunState,
    pub fan: bool,
    pub fuel_pump: bool,
    pub glow_plug: bool,
    pub flame: bool,
    /// `None` when the heater reports no fault (0xFF on the wire).
    pub error: Option<u8>,
    /// Rises to about 3.5 during warm-up with the setting at 1.4, then
    /// eases back, so it tracks the burn rate more than the setting.
    pub live_power_kw: f32,
    /// Byte 6 of the report. Thought to be a temperature, but it stayed at
    /// 97 through a whole run on a DHG001, so do not build logic on it.
    pub heater_temp_c: i8,
    pub set_power_kw: f32,
    pub set_temp_c: u8,
    /// Reads 0 while the heater is idle and 12 to 13 V while it runs.
    pub supply_volts: f32,
    pub schedule_enabled: bool,
}

impl Status {
    /// Parses a status report. Returns `None` if the packet is not a status
    /// report, has the wrong length, or fails its checksum.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() != STATUS_LEN || data[1] != TYPE_STATUS {
            return None;
        }
        if checksum(&data[..STATUS_LEN - 1]) != data[STATUS_LEN - 1] {
            return None;
        }

        let run = data[2];
        let state = match run & 0b11 {
            0b00 => RunState::Off,
            0b01 => RunState::On,
            0b10 => RunState::ShuttingDown,
            _ => RunState::Starting,
        };

        Some(Self {
            state,
            fan: run & (1 << 2) != 0,
            fuel_pump: run & (1 << 3) != 0,
            glow_plug: run & (1 << 4) != 0,
            flame: run & (1 << 5) != 0,
            error: (data[3] != 0xFF).then_some(data[3]),
            live_power_kw: f32::from(data[5] as i8) * 0.1,
            heater_temp_c: data[6] as i8,
            set_power_kw: f32::from(data[12]) * 0.1,
            set_temp_c: data[13],
            supply_volts: f32::from(data[14]) * 0.2,
            schedule_enabled: data[11] & (1 << 1) != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A status report captured from a real heater while it was switched off.
    const IDLE: [u8; 20] = [
        0x80, 0x41, 0x00, 0xff, 0x00, 0x00, 0x60, 0x00, 0x00, 0x00, 0x00, 0x02, 0x0e, 0x19, 0x00,
        0x18, 0x00, 0x1c, 0x00, 0x7d,
    ];

    #[test]
    fn frames_match_the_vendor_app() {
        assert_eq!(frame(CMD_VERSION, &[]), [0x00, 0x0e, 0x0e]);
        // Switch on, switch off, and 1.4 kW, as the app builds them.
        use crate::actuator::Switch;
        assert_eq!(switch_frame(Switch::On), [0x00, 0x05, 0x01, 0x06]);
        assert_eq!(switch_frame(Switch::Off), [0x00, 0x05, 0x00, 0x05]);
        assert_eq!(frame(0x07, &[14]), [0x00, 0x07, 0x0e, 0x15]);
    }

    #[test]
    fn clock_frame_layout() {
        let t = crate::clock::LocalTime::parse_compact("261110165312").unwrap();
        // 00 0A 1A 0B 0A 10 35 0C, checksum 0x8A.
        assert_eq!(clock_frame(&t), [0x00, 0x0a, 26, 11, 10, 16, 53, 12, 0x8a]);
    }

    #[test]
    fn parses_a_captured_idle_report() {
        let status = Status::parse(&IDLE).expect("valid report");
        assert_eq!(status.state, RunState::Off);
        assert!(!status.fan && !status.fuel_pump && !status.glow_plug && !status.flame);
        assert_eq!(status.error, None);
        assert_eq!(status.set_temp_c, 25);
        assert!((status.set_power_kw - 1.4).abs() < 0.01);
        assert!(status.schedule_enabled);
    }

    #[test]
    fn rejects_a_corrupt_report() {
        let mut bad = IDLE;
        bad[12] ^= 0x01;
        assert_eq!(Status::parse(&bad), None);
        assert_eq!(Status::parse(&IDLE[..19]), None);
    }
}
