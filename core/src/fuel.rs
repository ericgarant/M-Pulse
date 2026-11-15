//! Fuel gauge for the heater's tank, worked out without a sensor.
//!
//! The heater does not report its fuel level or how much its pump moves, so
//! the hub keeps a running tally instead:
//!
//! 1. You tell it the tank is full (or how many litres are in it).
//! 2. While the heater burns, fuel is taken off at the rate BougeRV's manual
//!    gives for the power setting: 0.18 L/h at 1.4 kW up to 0.53 L/h at
//!    5.0 kW, in a straight line between them.
//! 3. When you fill up to full again and say how many litres went in, the
//!    gauge compares that with what it thought was burned and scales its
//!    rate to match ([`Gauge::correction`]). This is what turns the manual's
//!    round figures into numbers for your heater.
//!
//! Burning the hub did not see cannot be counted. If the heater link drops
//! while the heater burns, the gauge keeps taking fuel off at the last
//! power setting for up to [`GAP_LIMIT`] (better to read low than high) and
//! marks the reading uncertain until the next fill-up. A heater found
//! already burning after a restart or a reconnect marks it uncertain too.
//! A burn started and finished entirely while the hub was off or out of
//! range leaves no trace and is not counted.
//!
//! Two levels act on the result: a warning (default 15 %), and a reserve
//! (default 15 %) at which the hub stops a burning heater with its normal
//! Off, so it runs its full cool-down, and refuses to start it. Both can be
//! changed from the dashboard. Until a first fill-up is entered the level
//! is unknown and neither acts.

use core::ops::RangeInclusive;
use core::time::Duration;

use crate::protocol::{RunState, Status};

/// The tank on Eric's heater.
pub const DEFAULT_TANK_L: f32 = 5.0;
/// Tank sizes the dashboard may set.
pub const TANK_RANGE_L: RangeInclusive<f32> = 1.0..=60.0;

/// Lowest and highest warning or reserve level the dashboard may set.
pub const MIN_PCT: u8 = 5;
pub const MAX_PCT: u8 = 50;

/// Consumption from BougeRV's manual for the 5 kW heater: litres an hour at
/// the lowest and highest power. Settings outside 1.4 to 5.0 kW follow the
/// same straight line, within the hub's own 1.0 to 5.0 kW limits.
const LOW_KW: f32 = 1.4;
const LOW_L_PER_H: f32 = 0.18;
const HIGH_KW: f32 = 5.0;
const HIGH_L_PER_H: f32 = 0.53;
const KW_RANGE: RangeInclusive<f32> = 1.0..=5.0;

/// How far a fill-up may scale the manual's rate. A measured ratio outside
/// this is far more likely a typing slip than a real pump.
pub const CORRECTION_RANGE: RangeInclusive<f32> = 0.5..=2.0;
/// The gauge must have counted at least this much since the last fill to
/// full before a fill-up is used to correct the rate: on a small burn, a
/// tenth of a litre misread on the jerry can would swing it wildly.
pub const MIN_CALIBRATION_L: f32 = 1.0;
/// Weight of the newest fill-up in the correction. Half keeps one odd
/// fill-up from undoing the others while still settling in a few refills.
const CALIBRATION_WEIGHT: f32 = 0.5;

/// How long fuel is still taken off after the heater link drops mid-burn.
pub const GAP_LIMIT: Duration = Duration::from_secs(3600);

/// Litres an hour the manual gives for a power setting, before correction.
/// A setting that is not a number is treated as full power, so a bad value
/// can only make the gauge read low.
pub fn litres_per_hour(set_kw: f32) -> f32 {
    let kw = if set_kw.is_finite() {
        set_kw.clamp(*KW_RANGE.start(), *KW_RANGE.end())
    } else {
        HIGH_KW
    };
    LOW_L_PER_H + (kw - LOW_KW) * (HIGH_L_PER_H - LOW_L_PER_H) / (HIGH_KW - LOW_KW)
}

/// Whether a status report shows the heater taking fuel: started or on, with
/// the pump running or a flame. The glow plug's warm-up before the pump
/// starts, and the cool-down after it stops, do not count.
pub fn consumes_fuel(status: &Status) -> bool {
    matches!(status.state, RunState::On | RunState::Starting) && (status.fuel_pump || status.flame)
}

/// The levels the dashboard sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub tank_l: f32,
    /// Warn at or below this share of the tank.
    pub warn_pct: u8,
    /// Stop the heater and refuse starts at or below this share of the tank.
    pub reserve_pct: u8,
    /// Whether the reserve stops and blocks the heater. The warning shows
    /// either way.
    pub reserve_stop: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tank_l: DEFAULT_TANK_L,
            warn_pct: 15,
            reserve_pct: 15,
            reserve_stop: true,
        }
    }
}

impl Config {
    pub fn with_warn(self, pct: u8) -> Option<Self> {
        (MIN_PCT..=MAX_PCT).contains(&pct).then_some(Self {
            warn_pct: pct,
            ..self
        })
    }

    pub fn with_reserve(self, pct: u8) -> Option<Self> {
        (MIN_PCT..=MAX_PCT).contains(&pct).then_some(Self {
            reserve_pct: pct,
            ..self
        })
    }

    pub fn with_tank(self, litres: f32) -> Option<Self> {
        TANK_RANGE_L.contains(&litres).then_some(Self {
            tank_l: litres,
            ..self
        })
    }

    fn reserve_l(&self) -> f32 {
        self.tank_l * f32::from(self.reserve_pct) / 100.0
    }
}

/// What the hub knows about the tank. Saved to flash, so it survives a
/// power cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gauge {
    /// Litres in the tank, or `None` until a fill-up is entered.
    pub level_l: Option<f32>,
    /// Litres the manual's rate says were burned since the last fill to
    /// full, before correction. Compared with the litres that go in at the
    /// next fill to full.
    pub counted_l: f32,
    /// Whether the next fill to full can correct the rate: the last fill
    /// was to full and nothing was missed since.
    pub can_calibrate: bool,
    /// Burning may have gone uncounted since the last fill-up.
    pub uncertain: bool,
    /// Scales the manual's rate to this heater (1.0 = as the manual says).
    pub correction: f32,
    /// Fill-ups that have corrected the rate so far.
    pub calibrations: u8,
}

impl Default for Gauge {
    fn default() -> Self {
        Self {
            level_l: None,
            counted_l: 0.0,
            can_calibrate: false,
            uncertain: false,
            correction: 1.0,
            calibrations: 0,
        }
    }
}

/// The result of a fill to full.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fill {
    /// The rate was corrected: the new correction, and the one this
    /// fill-up alone measured.
    Calibrated { correction: f32, measured: f32 },
    /// No litres were given, or the gauge cannot use them (see the reason).
    Plain(Option<&'static str>),
}

impl Gauge {
    /// Takes `model_l` litres (at the manual's rate) off the tank.
    pub fn burn(&mut self, model_l: f32) {
        if !(model_l.is_finite() && model_l > 0.0) {
            return;
        }
        self.counted_l += model_l;
        if let Some(level) = self.level_l.as_mut() {
            *level = (*level - model_l * self.correction).max(0.0);
        }
    }

    /// Burning may have been missed: flags the reading until the next
    /// fill-up and keeps that fill-up from correcting the rate.
    pub fn mark_uncertain(&mut self) {
        self.uncertain = true;
        self.can_calibrate = false;
    }

    /// The tank was filled to the top. `added_l`, if given, is how many
    /// litres went in; when the gauge counted every burn since the last fill
    /// to full, it corrects the rate with them.
    pub fn fill_to_full(&mut self, config: &Config, added_l: Option<f32>) -> Fill {
        let result = match added_l {
            None => Fill::Plain(None),
            Some(_) if !self.can_calibrate => Fill::Plain(Some(
                "the last fill-up was not to full, or some burning was missed since",
            )),
            Some(_) if self.counted_l < MIN_CALIBRATION_L => Fill::Plain(Some(
                "too little has been burned since the last fill-up to measure",
            )),
            Some(added) => {
                let measured = added / self.counted_l;
                if CORRECTION_RANGE.contains(&measured) {
                    let correction = (self.correction * (1.0 - CALIBRATION_WEIGHT)
                        + measured * CALIBRATION_WEIGHT)
                        .clamp(*CORRECTION_RANGE.start(), *CORRECTION_RANGE.end());
                    self.correction = correction;
                    self.calibrations = self.calibrations.saturating_add(1);
                    Fill::Calibrated {
                        correction,
                        measured,
                    }
                } else {
                    Fill::Plain(Some(
                        "the litres added are too far from the estimate to trust",
                    ))
                }
            }
        };
        self.level_l = Some(config.tank_l);
        self.counted_l = 0.0;
        self.can_calibrate = true;
        self.uncertain = false;
        result
    }

    /// Sets the level by hand, for a part fill or a reading off the tank.
    /// The next fill to full cannot correct the rate, because the gauge no
    /// longer knows how much was burned since the tank was last full.
    pub fn set_level(&mut self, config: &Config, litres: f32) {
        self.level_l = Some(litres.clamp(0.0, config.tank_l));
        self.counted_l = 0.0;
        self.can_calibrate = false;
        self.uncertain = false;
    }

    /// Forgets the rate correction and goes back to the manual's figures.
    pub fn reset_correction(&mut self) {
        self.correction = 1.0;
        self.calibrations = 0;
    }

    /// Share of the tank left, 0 to 100, if known.
    pub fn percent(&self, config: &Config) -> Option<f32> {
        let level = self.level_l?;
        Some((level / config.tank_l * 100.0).clamp(0.0, 100.0))
    }

    /// Litres an hour this heater burns at `set_kw`, with the correction.
    pub fn rate(&self, set_kw: f32) -> f32 {
        litres_per_hour(set_kw) * self.correction
    }

    /// Hours of burning at `set_kw` until the heater stops for the reserve
    /// (or runs dry when the reserve stop is off).
    pub fn hours_left(&self, config: &Config, set_kw: f32) -> Option<f32> {
        let floor = if config.reserve_stop {
            config.reserve_l()
        } else {
            0.0
        };
        let usable = (self.level_l? - floor).max(0.0);
        Some(usable / self.rate(set_kw))
    }

    /// Whether the level is at or below the warning.
    pub fn low(&self, config: &Config) -> bool {
        self.at_or_below(config, config.warn_pct)
    }

    /// Whether the reserve must stop the heater and block starts.
    pub fn at_reserve(&self, config: &Config) -> bool {
        config.reserve_stop && self.at_or_below(config, config.reserve_pct)
    }

    /// Whether the level is at or below `pct` of the tank. A hair of slack,
    /// so 0.75 L in a 5 L tank counts as 15 % despite rounding.
    fn at_or_below(&self, config: &Config, pct: u8) -> bool {
        self.percent(config)
            .is_some_and(|p| p <= f32::from(pct) + 1e-3)
    }
}

/// What the hub saw of the heater since the last step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seen {
    /// A fresh status report: the power setting if the heater is taking
    /// fuel, `None` if not.
    Burning(f32),
    Idle,
    /// No fresh report: the link is down or the heater went quiet.
    Nothing,
}

impl Seen {
    pub fn from_status(status: Option<&Status>) -> Self {
        match status {
            Some(st) if consumes_fuel(st) => Seen::Burning(st.set_power_kw),
            Some(_) => Seen::Idle,
            None => Seen::Nothing,
        }
    }
}

/// Feeds the heater's reports into a [`Gauge`]. Lives only in memory.
#[derive(Clone, Copy, Debug)]
pub struct Meter {
    /// The power setting the heater was last seen burning at, while it may
    /// still be burning.
    last_burning: Option<f32>,
    /// How long fuel has been taken off without a fresh report.
    gap: Duration,
    /// No fresh report since the hub started or the link last dropped.
    blind: bool,
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            last_burning: None,
            gap: Duration::ZERO,
            blind: true,
        }
    }
}

impl Meter {
    /// One step of `elapsed` time.
    pub fn step(&mut self, gauge: &mut Gauge, seen: Seen, elapsed: Duration) {
        let hours = elapsed.as_secs_f32() / 3600.0;
        match seen {
            Seen::Burning(kw) => {
                // Burning, and the hub did not see it start: it may have
                // burned for a while already.
                if self.blind && self.last_burning.is_none() {
                    gauge.mark_uncertain();
                }
                gauge.burn(litres_per_hour(kw) * hours);
                self.last_burning = Some(kw);
                self.gap = Duration::ZERO;
                self.blind = false;
            }
            Seen::Idle => {
                self.last_burning = None;
                self.gap = Duration::ZERO;
                self.blind = false;
            }
            Seen::Nothing => {
                self.blind = true;
                let Some(kw) = self.last_burning else {
                    return;
                };
                gauge.mark_uncertain();
                if self.gap >= GAP_LIMIT {
                    return;
                }
                let left = GAP_LIMIT - self.gap;
                let counted = elapsed.min(left);
                gauge.burn(litres_per_hour(kw) * counted.as_secs_f32() / 3600.0);
                self.gap += counted;
            }
        }
    }
}

/// Everything saved to flash: the gauge and the dashboard's levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Saved {
    pub config: Config,
    pub gauge: Gauge,
}

impl Saved {
    /// Bytes in [`Saved::encode`]'s output.
    pub const LEN: usize = 32;
    const VERSION: u8 = 1;

    /// Encodes for flash: a version, the fields, `seq` (to tell the newer
    /// of two saved copies) and a checksum.
    pub fn encode(&self, seq: u32) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0] = Self::VERSION;
        let flags = u8::from(self.gauge.level_l.is_some())
            | u8::from(self.gauge.can_calibrate) << 1
            | u8::from(self.gauge.uncertain) << 2
            | u8::from(self.config.reserve_stop) << 3;
        out[1] = flags;
        out[2] = self.config.warn_pct;
        out[3] = self.config.reserve_pct;
        out[4..8].copy_from_slice(&self.config.tank_l.to_le_bytes());
        out[8..12].copy_from_slice(&self.gauge.level_l.unwrap_or(0.0).to_le_bytes());
        out[12..16].copy_from_slice(&self.gauge.counted_l.to_le_bytes());
        out[16..20].copy_from_slice(&self.gauge.correction.to_le_bytes());
        out[20] = self.gauge.calibrations;
        // 21..24 spare.
        out[24..28].copy_from_slice(&seq.to_le_bytes());
        let sum = fnv1a(&out[..28]);
        out[28..32].copy_from_slice(&sum.to_le_bytes());
        out
    }

    /// Decodes a saved copy and its `seq`. Returns `None` for anything
    /// damaged, from another version, or out of range.
    pub fn decode(bytes: &[u8]) -> Option<(Self, u32)> {
        if bytes.len() != Self::LEN || bytes[0] != Self::VERSION {
            return None;
        }
        let word = |at: usize| [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        if fnv1a(&bytes[..28]) != u32::from_le_bytes(word(28)) {
            return None;
        }
        let flags = bytes[1];
        let config = Config::default()
            .with_tank(f32::from_le_bytes(word(4)))?
            .with_warn(bytes[2])?
            .with_reserve(bytes[3])?;
        let config = Config {
            reserve_stop: flags & 1 << 3 != 0,
            ..config
        };
        let level = f32::from_le_bytes(word(8));
        let counted = f32::from_le_bytes(word(12));
        let correction = f32::from_le_bytes(word(16));
        let sane = level.is_finite()
            && (0.0..=config.tank_l).contains(&level)
            && counted.is_finite()
            && counted >= 0.0
            && CORRECTION_RANGE.contains(&correction);
        if !sane {
            return None;
        }
        let gauge = Gauge {
            level_l: (flags & 1 != 0).then_some(level),
            counted_l: counted,
            can_calibrate: flags & 1 << 1 != 0,
            uncertain: flags & 1 << 2 != 0,
            correction,
            calibrations: bytes[20],
        };
        Some((Self { config, gauge }, u32::from_le_bytes(word(24))))
    }

    /// Whether this differs enough from the last saved copy to be written:
    /// any setting or flag, or the level by `LEVEL_STEP_L`. Writing every
    /// few seconds of burn would wear the flash for no gain.
    pub fn worth_saving(&self, last: &Saved) -> bool {
        const LEVEL_STEP_L: f32 = 0.02;
        let (a, b) = (self.gauge, last.gauge);
        let level_moved = match (a.level_l, b.level_l) {
            (Some(x), Some(y)) => (x - y).abs() >= LEVEL_STEP_L,
            (None, None) => false,
            _ => true,
        };
        self.config != last.config
            || level_moved
            || a.can_calibrate != b.can_calibrate
            || a.uncertain != b.uncertain
            || a.correction != b.correction
            || a.calibrations != b.calibrations
            || (a.counted_l == 0.0) != (b.counted_l == 0.0)
    }
}

fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn full() -> (Config, Gauge) {
        let c = Config::default();
        let mut g = Gauge::default();
        g.fill_to_full(&c, None);
        (c, g)
    }

    fn status(state: RunState, pump: bool, flame: bool) -> Status {
        Status {
            state,
            fan: true,
            fuel_pump: pump,
            glow_plug: false,
            flame,
            error: None,
            live_power_kw: 0.0,
            heater_temp_c: 0,
            set_power_kw: 2.0,
            set_temp_c: 20,
            supply_volts: 12.8,
            schedule_enabled: false,
        }
    }

    #[test]
    fn rate_follows_the_manual() {
        assert!(close(litres_per_hour(1.4), 0.18));
        assert!(close(litres_per_hour(5.0), 0.53));
        assert!(close(litres_per_hour(3.2), 0.355));
        // The hub's limits, and a bad value read as full power.
        assert!(close(litres_per_hour(9.0), 0.53));
        assert!(litres_per_hour(1.0) > 0.1 && litres_per_hour(1.0) < 0.18);
        assert!(close(litres_per_hour(f32::NAN), 0.53));
    }

    #[test]
    fn only_a_heater_taking_fuel_counts() {
        assert!(consumes_fuel(&status(RunState::On, true, true)));
        assert!(consumes_fuel(&status(RunState::On, false, true)));
        assert!(consumes_fuel(&status(RunState::Starting, true, false)));
        // Glow plug warm-up, before the pump starts.
        assert!(!consumes_fuel(&status(RunState::Starting, false, false)));
        assert!(!consumes_fuel(&status(RunState::ShuttingDown, false, true)));
        assert!(!consumes_fuel(&status(RunState::Off, false, false)));
    }

    #[test]
    fn unknown_until_the_first_fill_up() {
        let c = Config::default();
        let mut g = Gauge::default();
        g.burn(1.0);
        assert_eq!(g.percent(&c), None);
        assert!(!g.low(&c) && !g.at_reserve(&c));
        assert_eq!(g.hours_left(&c, 2.0), None);
    }

    #[test]
    fn burning_takes_fuel_off() {
        let (c, mut g) = full();
        let mut m = Meter::default();
        m.step(&mut g, Seen::Idle, Duration::from_secs(5));
        for _ in 0..10 {
            m.step(&mut g, Seen::Burning(5.0), HOUR);
        }
        // 5.3 L by the manual's rate, but never below empty.
        assert_eq!(g.level_l, Some(0.0));
        assert!(!g.uncertain, "every minute of it was seen");
        let (c2, mut g2) = full();
        m.step(&mut g2, Seen::Burning(1.4), HOUR);
        assert!(close(g2.level_l.unwrap(), 4.82));
        assert!(close(g2.percent(&c2).unwrap(), 96.4));
        let _ = c;
    }

    #[test]
    fn warning_and_reserve() {
        let (c, mut g) = full();
        g.set_level(&c, 0.8);
        assert!(!g.low(&c) && !g.at_reserve(&c));
        g.set_level(&c, 0.75);
        assert!(g.low(&c) && g.at_reserve(&c));
        let off = Config {
            reserve_stop: false,
            ..c
        };
        assert!(g.low(&off) && !g.at_reserve(&off));
        let split = c.with_warn(25).unwrap().with_reserve(10).unwrap();
        g.set_level(&split, 1.0);
        assert!(g.low(&split) && !g.at_reserve(&split));
        g.set_level(&split, 0.5);
        assert!(g.at_reserve(&split));
    }

    #[test]
    fn hours_left_stop_at_the_reserve() {
        let (c, g) = full();
        // 5 L less the 0.75 L reserve, at 0.18 L/h.
        assert!(close(g.hours_left(&c, 1.4).unwrap(), 4.25 / 0.18));
        let off = Config {
            reserve_stop: false,
            ..c
        };
        assert!(close(g.hours_left(&off, 1.4).unwrap(), 5.0 / 0.18));
    }

    #[test]
    fn a_fill_up_corrects_the_rate() {
        let (c, mut g) = full();
        // The manual's rate says 2 L were burned; 2.4 L went back in.
        g.burn(2.0);
        let fill = g.fill_to_full(&c, Some(2.4));
        assert_eq!(
            fill,
            Fill::Calibrated {
                correction: 1.1,
                measured: 1.2
            }
        );
        assert_eq!(g.calibrations, 1);
        assert_eq!(g.level_l, Some(5.0));
        // The next litre of the manual's rate now takes 1.1 L off.
        g.burn(1.0);
        assert!(close(g.level_l.unwrap(), 3.9));
        assert!(close(g.rate(1.4), 0.198));
        // A second fill-up that agrees moves it the rest of the way.
        let fill = g.fill_to_full(&c, Some(1.2));
        assert!(matches!(fill, Fill::Calibrated { correction, .. } if close(correction, 1.15)));
    }

    #[test]
    fn a_fill_up_it_cannot_trust_leaves_the_rate() {
        let (c, mut g) = full();
        g.burn(0.5);
        assert!(matches!(
            g.fill_to_full(&c, Some(0.6)),
            Fill::Plain(Some(_))
        ));
        assert_eq!(g.correction, 1.0);

        g.burn(2.0);
        // A typing slip: 20 L into a 5 L tank's worth of burning.
        assert!(matches!(
            g.fill_to_full(&c, Some(20.0)),
            Fill::Plain(Some(_))
        ));
        assert_eq!(g.correction, 1.0);

        g.burn(2.0);
        g.mark_uncertain();
        assert!(matches!(
            g.fill_to_full(&c, Some(2.0)),
            Fill::Plain(Some(_))
        ));
        assert!(!g.uncertain, "a fill-up clears it");

        g.set_level(&c, 3.0);
        g.burn(2.0);
        assert!(
            matches!(g.fill_to_full(&c, Some(2.0)), Fill::Plain(Some(_))),
            "the tank was not full before, so the litres added say nothing"
        );
        assert_eq!(g.correction, 1.0);
        assert_eq!(g.fill_to_full(&c, None), Fill::Plain(None));
    }

    #[test]
    fn a_dropped_link_mid_burn_keeps_counting_for_an_hour() {
        let (_, mut g) = full();
        let mut m = Meter::default();
        m.step(&mut g, Seen::Idle, Duration::from_secs(5));
        m.step(&mut g, Seen::Burning(1.4), HOUR);
        for _ in 0..3 {
            m.step(&mut g, Seen::Nothing, HOUR);
        }
        // One hour seen, one counted blind, then nothing.
        assert!(close(g.level_l.unwrap(), 5.0 - 0.36));
        assert!(g.uncertain && !g.can_calibrate);
    }

    #[test]
    fn a_link_lost_while_idle_counts_nothing() {
        let (_, mut g) = full();
        let mut m = Meter::default();
        m.step(&mut g, Seen::Idle, Duration::from_secs(5));
        m.step(&mut g, Seen::Nothing, HOUR);
        m.step(&mut g, Seen::Idle, Duration::from_secs(5));
        assert_eq!(g.level_l, Some(5.0));
        assert!(!g.uncertain);
    }

    #[test]
    fn found_burning_is_uncertain() {
        // After a hub restart.
        let (_, mut g) = full();
        let mut m = Meter::default();
        m.step(&mut g, Seen::Burning(2.0), Duration::from_secs(5));
        assert!(g.uncertain);

        // After a reconnect: idle when the link dropped, burning when back.
        let (_, mut g) = full();
        let mut m = Meter::default();
        m.step(&mut g, Seen::Idle, Duration::from_secs(5));
        m.step(&mut g, Seen::Nothing, Duration::from_secs(5));
        m.step(&mut g, Seen::Burning(2.0), Duration::from_secs(5));
        assert!(g.uncertain);

        // Started while watched.
        let (_, mut g) = full();
        let mut m = Meter::default();
        m.step(&mut g, Seen::Idle, Duration::from_secs(5));
        m.step(&mut g, Seen::Burning(2.0), Duration::from_secs(5));
        assert!(!g.uncertain);
    }

    #[test]
    fn settings_are_checked() {
        let c = Config::default();
        assert!(c.with_warn(5).is_some() && c.with_warn(50).is_some());
        assert!(c.with_warn(4).is_none() && c.with_reserve(51).is_none());
        assert!(c.with_tank(0.5).is_none() && c.with_tank(61.0).is_none());
        assert!(c.with_tank(f32::NAN).is_none());
        let (c, mut g) = full();
        g.set_level(&c, 9.0);
        assert_eq!(g.level_l, Some(5.0), "never more than the tank holds");
    }

    #[test]
    fn saved_round_trip() {
        let (c, mut g) = full();
        g.burn(1.3);
        g.mark_uncertain();
        g.correction = 1.07;
        g.calibrations = 2;
        let config = c.with_warn(20).unwrap().with_reserve(10).unwrap();
        let saved = Saved { config, gauge: g };
        let bytes = saved.encode(41);
        assert_eq!(Saved::decode(&bytes), Some((saved, 41)));

        let unknown = Saved {
            config: Config::default(),
            gauge: Gauge::default(),
        };
        assert_eq!(Saved::decode(&unknown.encode(0)), Some((unknown, 0)));
    }

    #[test]
    fn damaged_saves_are_refused() {
        let (c, g) = full();
        let bytes = Saved {
            config: c,
            gauge: g,
        }
        .encode(7);
        for i in 0..Saved::LEN {
            let mut bad = bytes;
            bad[i] ^= 0x10;
            assert_eq!(Saved::decode(&bad), None, "byte {i}");
        }
        assert_eq!(Saved::decode(&bytes[..31]), None);
    }

    #[test]
    fn small_level_moves_are_not_saved() {
        let (c, g) = full();
        let last = Saved {
            config: c,
            gauge: g,
        };
        let mut now = last;
        now.gauge.burn(0.01);
        // counted_l went from 0 to non-zero, which is saved once.
        assert!(now.worth_saving(&last));
        let last = now;
        now.gauge.burn(0.01);
        assert!(!now.worth_saving(&last));
        now.gauge.burn(0.011);
        assert!(now.worth_saving(&last));
        let mut flagged = last;
        flagged.gauge.mark_uncertain();
        assert!(flagged.worth_saving(&last));
    }
}
