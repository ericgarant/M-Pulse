//! The Sensirion SHT3x (SHT30, SHT31, SHT35) cabin sensor: commands and
//! decoding its 6-byte measurement.
//!
//! The hub talks to the sensor over I2C (`hub/src/sht3x.rs`); the command
//! bytes, the checksum and the conversion to °C and % RH live here so they
//! can be tested on any computer.
//!
//! Measurement frame: temperature (2 bytes, big-endian), its CRC, humidity
//! (2 bytes), its CRC. Each CRC is CRC-8 with polynomial 0x31 and initial
//! value 0xFF, over the two bytes before it (datasheet section 4.12).

/// I2C address with the ADDR pin low. Most breakout boards tie it low.
pub const ADDR_LOW: u8 = 0x44;
/// I2C address with the ADDR pin high.
pub const ADDR_HIGH: u8 = 0x45;
/// Both addresses, in the order the hub looks for the sensor.
pub const ADDRESSES: [u8; 2] = [ADDR_LOW, ADDR_HIGH];

/// Single-shot measurement, high repeatability, clock stretching off. The
/// sensor does not answer reads until the measurement is done, so the host
/// waits [`MEASURE_TIME_MS`] and then reads the frame.
pub const CMD_MEASURE: [u8; 2] = [0x24, 0x00];
/// Longest a high-repeatability measurement takes is 15.5 ms; allow margin.
pub const MEASURE_TIME_MS: u32 = 20;
/// Soft reset: back to the power-up state, heater off.
pub const CMD_SOFT_RESET: [u8; 2] = [0x30, 0xA2];
/// The sensor is ready again at most 1.5 ms after a soft reset.
pub const RESET_TIME_MS: u32 = 2;

/// The SHT3x is rated for -40 to 125 °C. A cabin reading outside this
/// range is a bad frame, not weather (and matches what the sensor node's
/// packets may carry, see `wire`).
const TEMP_RANGE: core::ops::RangeInclusive<f32> = -40.0..=85.0;

/// Why a frame was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// A CRC did not match: a corrupted transfer.
    Crc,
    /// The CRCs matched but the temperature cannot be a cabin's.
    Implausible,
}

/// CRC-8 used by Sensirion: polynomial 0x31 (x⁸ + x⁵ + x⁴ + 1), initial
/// value 0xFF, no reflection, no final XOR.
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0xFF, |mut crc, byte| {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x31
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// Decodes a measurement frame into (temperature in °C, relative humidity
/// in %).
///
/// Humidity is clamped to 0..=100 %: in condensing air the sensor can read
/// a little over 100 %, which is real, not an error.
pub fn decode(frame: [u8; 6]) -> Result<(f32, f32), FrameError> {
    let [t_hi, t_lo, t_crc, rh_hi, rh_lo, rh_crc] = frame;
    if crc8(&[t_hi, t_lo]) != t_crc || crc8(&[rh_hi, rh_lo]) != rh_crc {
        return Err(FrameError::Crc);
    }
    let raw_t = f32::from(u16::from_be_bytes([t_hi, t_lo]));
    let raw_rh = f32::from(u16::from_be_bytes([rh_hi, rh_lo]));
    // Datasheet section 4.13: T = -45 + 175 · raw / (2¹⁶ - 1),
    // RH = 100 · raw / (2¹⁶ - 1).
    let temperature = -45.0 + 175.0 * raw_t / 65535.0;
    let humidity = (100.0 * raw_rh / 65535.0).clamp(0.0, 100.0);
    if !TEMP_RANGE.contains(&temperature) {
        return Err(FrameError::Implausible);
    }
    Ok((temperature, humidity))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a frame with correct CRCs from raw values.
    fn frame(raw_t: u16, raw_rh: u16) -> [u8; 6] {
        let [t_hi, t_lo] = raw_t.to_be_bytes();
        let [rh_hi, rh_lo] = raw_rh.to_be_bytes();
        [
            t_hi,
            t_lo,
            crc8(&[t_hi, t_lo]),
            rh_hi,
            rh_lo,
            crc8(&[rh_hi, rh_lo]),
        ]
    }

    #[test]
    fn crc_matches_the_datasheet_example() {
        // Datasheet table 19: CRC of 0xBEEF is 0x92.
        assert_eq!(crc8(&[0xBE, 0xEF]), 0x92);
    }

    #[test]
    fn decodes_a_room_reading() {
        // 0x6666 ≈ 25 °C, 0x6666 = 40 % RH.
        let (t, rh) = decode(frame(0x6666, 0x6666)).unwrap();
        assert!((t - 25.0).abs() < 0.01, "{t}");
        assert!((rh - 40.0).abs() < 0.01, "{rh}");
    }

    #[test]
    fn decodes_below_freezing() {
        // -10 °C: raw = (−10 + 45) / 175 · 65535 = 13107.
        let (t, _) = decode(frame(13107, 0x8000)).unwrap();
        assert!((t + 10.0).abs() < 0.01, "{t}");
    }

    #[test]
    fn clamps_humidity_to_100() {
        let (_, rh) = decode(frame(0x6666, 0xFFFF)).unwrap();
        assert_eq!(rh, 100.0);
    }

    #[test]
    fn rejects_a_bad_crc() {
        let mut f = frame(0x6666, 0x6666);
        f[2] ^= 1;
        assert_eq!(decode(f), Err(FrameError::Crc));
        let mut f = frame(0x6666, 0x6666);
        f[4] ^= 0x10;
        assert_eq!(decode(f), Err(FrameError::Crc));
    }

    #[test]
    fn rejects_frames_that_cannot_be_real() {
        // All zeros with good CRCs would read as -45 °C.
        assert_eq!(decode(frame(0, 0)), Err(FrameError::Implausible));
        // Full scale is 130 °C.
        assert_eq!(decode(frame(0xFFFF, 0x6666)), Err(FrameError::Implausible));
    }

    #[test]
    fn rejects_an_idle_bus() {
        // A bus that reads high throughout gives 0xFF everywhere, and
        // 0xFF is not the CRC of 0xFFFF.
        assert_eq!(decode([0xFF; 6]), Err(FrameError::Crc));
    }
}
