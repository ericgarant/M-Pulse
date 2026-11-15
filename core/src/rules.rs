//! The battery rules' settings as saved in flash, so the levels and the
//! on/off switches survive a restart.
//!
//! Only the settings are saved, never a rule's state: after a restart the
//! low-battery stop and the fridge saver look at the Bluetti afresh.
//!
//! The low-battery stop's on/off switch is not saved: it is a safety, so it
//! comes back on after every restart, with the saved levels. The fridge
//! saver's switch is saved.
//! The layout follows `fuel::Saved`: a version, the fields, a sequence
//! number to tell the newer of two copies, and a checksum.

use crate::eco_guard;
use crate::fuel::fnv1a;
use crate::heater_cutoff;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Saved {
    pub cutoff: heater_cutoff::Config,
    pub saver: eco_guard::Config,
}

impl Saved {
    /// What gets saved from the rules' current settings: the low-battery
    /// stop is always recorded as on.
    pub fn new(cutoff: heater_cutoff::Config, saver: eco_guard::Config) -> Self {
        Self {
            cutoff: heater_cutoff::Config {
                enabled: true,
                ..cutoff
            },
            saver,
        }
    }

    /// Bytes in [`Saved::encode`]'s output.
    pub const LEN: usize = 16;
    const VERSION: u8 = 1;

    pub fn encode(&self, seq: u32) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0] = Self::VERSION;
        // Bit 0 is kept free: the low-battery stop's switch is not saved.
        out[1] = u8::from(self.saver.enabled) << 1;
        out[2] = self.cutoff.stop_pct;
        out[3] = self.cutoff.resume_pct;
        out[4] = self.saver.low_pct;
        out[5] = self.saver.high_pct;
        // 6..8 spare.
        out[8..12].copy_from_slice(&seq.to_le_bytes());
        let sum = fnv1a(&out[..12]);
        out[12..16].copy_from_slice(&sum.to_le_bytes());
        out
    }

    /// Decodes a saved copy and its `seq`. Returns `None` for anything
    /// damaged, from another version, or with levels the dashboard could
    /// not have set.
    pub fn decode(bytes: &[u8]) -> Option<(Self, u32)> {
        if bytes.len() != Self::LEN || bytes[0] != Self::VERSION {
            return None;
        }
        let word = |at: usize| [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        if fnv1a(&bytes[..12]) != u32::from_le_bytes(word(12)) {
            return None;
        }
        let flags = bytes[1];
        let cutoff = heater_cutoff::Config::default().with_levels(bytes[2], bytes[3])?;
        let saver = eco_guard::Config {
            enabled: flags & 2 != 0,
            ..eco_guard::Config::default()
        }
        .with_levels(bytes[4], bytes[5])?;
        Some((Self { cutoff, saver }, u32::from_le_bytes(word(8))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Saved {
        Saved {
            cutoff: heater_cutoff::Config {
                enabled: true,
                stop_pct: 20,
                resume_pct: 35,
            },
            saver: eco_guard::Config {
                enabled: true,
                low_pct: 40,
                high_pct: 60,
            },
        }
    }

    #[test]
    fn round_trip() {
        let bytes = sample().encode(7);
        assert_eq!(Saved::decode(&bytes), Some((sample(), 7)));
    }

    #[test]
    fn the_low_battery_stop_always_comes_back_on() {
        let off = heater_cutoff::Config {
            enabled: false,
            stop_pct: 20,
            resume_pct: 30,
        };
        let saved = Saved::new(off, eco_guard::Config::default());
        let (back, _) = Saved::decode(&saved.encode(3)).unwrap();
        assert!(back.cutoff.enabled);
        assert_eq!((back.cutoff.stop_pct, back.cutoff.resume_pct), (20, 30));
    }

    #[test]
    fn defaults_round_trip() {
        let bytes = Saved::default().encode(0);
        assert_eq!(Saved::decode(&bytes), Some((Saved::default(), 0)));
    }

    #[test]
    fn rejects_a_damaged_copy() {
        let mut bytes = sample().encode(1);
        bytes[2] ^= 1;
        assert_eq!(Saved::decode(&bytes), None);
    }

    #[test]
    fn rejects_another_version_or_length() {
        let mut bytes = sample().encode(1);
        bytes[0] = 2;
        assert_eq!(Saved::decode(&bytes), None);
        assert_eq!(Saved::decode(&sample().encode(1)[..12]), None);
    }

    #[test]
    fn rejects_levels_out_of_range() {
        // A good checksum over levels the dashboard refuses: stop above 50 %.
        let mut bytes = sample().encode(1);
        bytes[2] = 60;
        bytes[3] = 70;
        let sum = fnv1a(&bytes[..12]);
        bytes[12..16].copy_from_slice(&sum.to_le_bytes());
        assert_eq!(Saved::decode(&bytes), None);
    }
}
