//! Cabin readings sent from the sensor node to the hub over UDP.
//!
//! One short text line per datagram, so it can be read with `nc -ul 4210`:
//!
//! ```text
//! MP1 <seq> <temperature in tenths of °C> <humidity in tenths of %>
//! ```
//!
//! For example `MP1 42 215 380` is reading number 42: 21.5 °C, 38.0 % RH.

/// UDP port the hub listens on. The sensor node broadcasts to it.
pub const PORT: u16 = 4210;

const TAG: &str = "MP1";

/// One reading as it travels between the boards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CabinPacket {
    /// Counts up by one per reading, so the hub can spot gaps and repeats.
    pub seq: u32,
    pub temperature_c: f32,
    pub humidity_pct: f32,
}

impl CabinPacket {
    pub fn encode(&self) -> String {
        format!(
            "{TAG} {} {} {}",
            self.seq,
            (self.temperature_c * 10.0).round() as i32,
            (self.humidity_pct * 10.0).round() as i32
        )
    }

    /// Parses a datagram. Returns `None` for anything that is not a
    /// well-formed packet with plausible values.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let text = core::str::from_utf8(data).ok()?;
        let mut fields = text.trim().split(' ');
        if fields.next()? != TAG {
            return None;
        }
        let seq = fields.next()?.parse().ok()?;
        let temp: i32 = fields.next()?.parse().ok()?;
        let rh: i32 = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        // The DHT11 covers 0 to 50 °C; allow a margin for other sensors.
        if !(-400..=850).contains(&temp) || !(0..=1000).contains(&rh) {
            return None;
        }
        Some(Self {
            seq,
            temperature_c: temp as f32 / 10.0,
            humidity_pct: rh as f32 / 10.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let p = CabinPacket {
            seq: 42,
            temperature_c: 21.5,
            humidity_pct: 38.0,
        };
        assert_eq!(p.encode(), "MP1 42 215 380");
        assert_eq!(CabinPacket::parse(p.encode().as_bytes()), Some(p));
    }

    #[test]
    fn handles_negative_and_trailing_newline() {
        let p = CabinPacket::parse(b"MP1 7 -35 900\n").unwrap();
        assert_eq!(p.seq, 7);
        assert!((p.temperature_c + 3.5).abs() < 1e-6);
    }

    #[test]
    fn rejects_bad_packets() {
        for bad in [
            &b"MP2 1 200 400"[..],
            b"MP1 1 200",
            b"MP1 1 200 400 9",
            b"MP1 x 200 400",
            b"MP1 1 2000 400",
            b"MP1 1 200 -1",
            b"\xff\xfe",
        ] {
            assert_eq!(CabinPacket::parse(bad), None, "{bad:?}");
        }
    }
}
