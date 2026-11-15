//! Decoding the DHT11's 5-byte frame.
//!
//! The hub reads the bits itself (`hub/src/dht.rs`); turning them into a
//! reading lives here so it can be tested on any computer.
//!
//! Frame: humidity, humidity tenths, temperature, temperature tenths,
//! checksum (low byte of the sum of the first four). Current DHT11 parts put
//! a tenth of a degree in byte 3 and flag a temperature below 0 °C with its
//! top bit; older parts always send 0 there, which decodes the same way.

/// Highest humidity a good frame can carry.
const MAX_RH: f32 = 100.0;
/// The DHT11 is rated for 0 to 50 °C; allow some margin either side, but
/// nothing a cabin cannot be.
const TEMP_RANGE: core::ops::RangeInclusive<f32> = -20.0..=60.0;

/// Why a frame was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    Checksum,
    /// The checksum matched but the values cannot be real. An all-zero frame
    /// (a line that read low throughout) passes the checksum, so this check
    /// matters: it would otherwise read as 0 °C and 0 % RH.
    Implausible,
}

/// Decodes a frame into (temperature in °C, relative humidity in %).
pub fn decode(frame: [u8; 5]) -> Result<(f32, f32), FrameError> {
    let [rh, rh_tenths, t, t_tenths, sum] = frame;
    let total = [rh, rh_tenths, t, t_tenths]
        .iter()
        .fold(0u8, |acc, b| acc.wrapping_add(*b));
    if total != sum {
        return Err(FrameError::Checksum);
    }
    let humidity = f32::from(rh) + f32::from(rh_tenths.min(9)) * 0.1;
    let magnitude = f32::from(t) + f32::from((t_tenths & 0x7F).min(9)) * 0.1;
    let temperature = if t_tenths & 0x80 != 0 {
        -magnitude
    } else {
        magnitude
    };
    // Real air is never at 0 % RH; a frame of zeros is a bad read.
    if humidity <= 0.0 || humidity > MAX_RH || !TEMP_RANGE.contains(&temperature) {
        return Err(FrameError::Implausible);
    }
    Ok((temperature, humidity))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_sum(b: [u8; 4]) -> [u8; 5] {
        let s = b.iter().fold(0u8, |a, x| a.wrapping_add(*x));
        [b[0], b[1], b[2], b[3], s]
    }

    #[test]
    fn whole_degrees() {
        assert_eq!(decode(with_sum([41, 0, 19, 0])), Ok((19.0, 41.0)));
    }

    #[test]
    fn tenths_of_a_degree() {
        let (t, rh) = decode(with_sum([41, 0, 18, 6])).unwrap();
        assert!((t - 18.6).abs() < 1e-4);
        assert_eq!(rh, 41.0);
    }

    #[test]
    fn below_freezing() {
        // -2.3 °C: sign in the top bit of the tenths byte.
        let (t, _) = decode(with_sum([60, 0, 2, 0x80 | 3])).unwrap();
        assert!((t + 2.3).abs() < 1e-4);
    }

    #[test]
    fn rejects_a_bad_checksum() {
        let mut f = with_sum([41, 0, 19, 0]);
        f[4] ^= 1;
        assert_eq!(decode(f), Err(FrameError::Checksum));
    }

    #[test]
    fn rejects_frames_that_cannot_be_real() {
        assert_eq!(decode([0; 5]), Err(FrameError::Implausible));
        assert_eq!(
            decode(with_sum([120, 0, 20, 0])),
            Err(FrameError::Implausible)
        );
        assert_eq!(
            decode(with_sum([40, 0, 90, 0])),
            Err(FrameError::Implausible)
        );
    }
}
