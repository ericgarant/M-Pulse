//! Wire format of the BougeRV DHG001 diesel heater's BLE protocol.
//!
//! Taken from the vendor's Android app (v2.6.2). Everything here is pure data
//! handling with no hardware access.
//!
//! Commands are written to the "RX cmd." characteristic as
//! `[0x00, command, payload.., checksum]`, where the checksum is the low byte
//! of the sum of all preceding bytes.
//!
//! Known commands that this firmware does not send yet:
//!
//! - `0x05`: switch, payload `1` for on and `0` for off
//! - `0x07`: power level, payload is kilowatts times ten (14 = 1.4 kW)
//! - `0x0A`: set the clock, payload `YY MM DD hh mm ss`

/// Ask for the firmware version. The app sends this right after connecting.
pub const CMD_VERSION: u8 = 0x0E;

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
///
/// The fields are only logged for now. The thermostat logic will read them.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    pub state: RunState,
    pub fan: bool,
    pub fuel_pump: bool,
    pub glow_plug: bool,
    pub flame: bool,
    /// `None` when the heater reports no fault (0xFF on the wire).
    pub error: Option<u8>,
    pub live_power_kw: f32,
    /// The heater's own temperature reading in degrees Celsius.
    pub heater_temp_c: i8,
    pub set_power_kw: f32,
    pub set_temp_c: u8,
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
