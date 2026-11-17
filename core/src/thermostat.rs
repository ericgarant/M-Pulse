//! Thermostat decision logic.
//!
//! Decides whether the heater should be running, given the cabin temperature
//! and the time. It is a plain state machine with no I/O: the caller feeds it
//! readings and acts on what it returns.
//!
//! Two mechanisms keep the heater from short-cycling, which wears out a
//! diesel heater's glow plug and leaves soot in the burner:
//!
//! - a deadband around the setpoint, so small temperature wobbles do nothing
//! - minimum on and off times, so every burn and every rest has a floor
//!
//! The deadband's width is a user setting (the dashboard's comfort band),
//! kept within [`BAND_MIN_C`] and [`BAND_MAX_C`] and saved in flash as a
//! [`Saved`].

use std::time::{Duration, Instant};

use crate::fuel::fnv1a;

/// Narrowest comfort band the dashboard may set, as the half-width in °C
/// (so ±0.5 °C, 1 °C from bottom to top). Cabin air wobbles by a few
/// tenths around the sensor, and the sensor node's DHT11 reads in whole
/// degrees, so anything narrower would mostly react to noise. The minimum on and off times still cap
/// the heater at one start every 20 minutes, whatever the band.
pub const BAND_MIN_C: f32 = 0.5;
/// Widest comfort band, as the half-width in °C (±3 °C, 6 °C in all).
pub const BAND_MAX_C: f32 = 3.0;
/// The dashboard changes the band in steps of this much.
pub const BAND_STEP_C: f32 = 0.5;

/// Parses a comfort band half-width from the dashboard (`1.5`). Returns it
/// only if it is within [`BAND_MIN_C`]..=[`BAND_MAX_C`] and on a
/// [`BAND_STEP_C`] step (to within rounding).
pub fn parse_band(text: &str) -> Option<f32> {
    let v: f32 = text.trim().parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    let steps = (v / BAND_STEP_C).round();
    let snapped = steps * BAND_STEP_C;
    let on_step = (v - snapped).abs() < 0.01;
    (on_step && (BAND_MIN_C..=BAND_MAX_C).contains(&snapped)).then_some(snapped)
}

/// The thermostat settings kept in flash, so a restart keeps them.
///
/// Today that is only the comfort band. The setpoint still goes back to the
/// default after a restart. The layout follows `rules::Saved`: a version,
/// the fields, a sequence number to tell the newer of two copies, and a
/// checksum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saved {
    /// The band's half-width in tenths of a degree, so it compares exactly.
    band_x10: u8,
}

impl Saved {
    /// Bytes in [`Saved::encode`]'s output.
    pub const LEN: usize = 12;
    const VERSION: u8 = 1;

    /// What gets saved from the thermostat's current settings.
    pub fn new(config: &Config) -> Self {
        // The dashboard keeps the band within 0.5..=3.0, so this fits a u8.
        let x10 = (config.hysteresis_c * 10.0).round().clamp(0.0, 255.0);
        Self {
            band_x10: x10 as u8,
        }
    }

    /// The comfort band's half-width in °C.
    pub fn hysteresis_c(&self) -> f32 {
        f32::from(self.band_x10) / 10.0
    }

    /// `config` with the saved settings applied.
    pub fn apply(&self, config: Config) -> Config {
        Config {
            hysteresis_c: self.hysteresis_c(),
            ..config
        }
    }

    pub fn encode(&self, seq: u32) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0] = Self::VERSION;
        out[1] = self.band_x10;
        // 2..4 spare (room for the setpoint later).
        out[4..8].copy_from_slice(&seq.to_le_bytes());
        let sum = fnv1a(&out[..8]);
        out[8..12].copy_from_slice(&sum.to_le_bytes());
        out
    }

    /// Decodes a saved copy and its `seq`. Returns `None` for anything
    /// damaged, from another version, or with a band the dashboard could not
    /// have set.
    pub fn decode(bytes: &[u8]) -> Option<(Self, u32)> {
        if bytes.len() != Self::LEN || bytes[0] != Self::VERSION {
            return None;
        }
        let word = |at: usize| [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        if fnv1a(&bytes[..8]) != u32::from_le_bytes(word(8)) {
            return None;
        }
        let band_x10 = bytes[1];
        // The same limits as `parse_band`, in tenths: 5..=30 in steps of 5.
        if !(5..=30).contains(&band_x10) || !band_x10.is_multiple_of(5) {
            return None;
        }
        Some((Self { band_x10 }, u32::from_le_bytes(word(4))))
    }
}

impl Default for Saved {
    fn default() -> Self {
        Self::new(&Config::default())
    }
}

/// Thermostat settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// Target cabin temperature in degrees Celsius.
    pub setpoint_c: f32,
    /// Half-width of the deadband. Heat is called for at or below
    /// `setpoint - hysteresis` and dropped at or above `setpoint + hysteresis`.
    pub hysteresis_c: f32,
    /// Once on, stay on at least this long.
    pub min_on: Duration,
    /// Once off, stay off at least this long.
    pub min_off: Duration,
    /// A reading older than this counts as no reading at all.
    pub sensor_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            setpoint_c: 18.0,
            hysteresis_c: 1.0,
            min_on: Duration::from_secs(10 * 60),
            min_off: Duration::from_secs(10 * 60),
            sensor_timeout: Duration::from_secs(2 * 60),
        }
    }
}

/// Why the thermostat wants what it wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// At or below the bottom of the deadband.
    TooCold,
    /// At or above the top of the deadband.
    WarmEnough,
    /// Inside the deadband, so the current state is kept.
    InDeadband,
    /// Warm enough to stop, but the minimum on time has not passed.
    HoldingMinOn,
    /// Cold enough to start, but the minimum off time has not passed.
    HoldingMinOff,
    /// No fresh temperature reading. Heat is always off in this case.
    SensorLost,
}

/// The thermostat's output for one update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Whether the heater should be running.
    pub heat: bool,
    pub reason: Reason,
}

/// A temperature and the moment it was measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub temperature_c: f32,
    pub taken_at: Instant,
}

/// The thermostat state machine.
#[derive(Clone, Debug)]
pub struct Thermostat {
    config: Config,
    heat: bool,
    /// When `heat` last changed. `None` until the first change, which lets
    /// the first call for heat after power-up through without a wait.
    changed_at: Option<Instant>,
}

impl Thermostat {
    /// Creates a thermostat that starts with heat off.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            heat: false,
            changed_at: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Changes the target temperature. Takes effect on the next update.
    pub fn set_setpoint(&mut self, setpoint_c: f32) {
        self.config.setpoint_c = setpoint_c;
    }

    /// Changes the comfort band's half-width. Takes effect on the next
    /// update, and leaves the heater's state and the minimum on and off
    /// timers alone: a narrower band can make the heater switch sooner,
    /// never before its minimum run or rest is over.
    pub fn set_hysteresis(&mut self, hysteresis_c: f32) {
        self.config.hysteresis_c = hysteresis_c;
    }

    /// Whether heat is currently called for.
    pub fn heat(&self) -> bool {
        self.heat
    }

    /// Takes over what the heater is actually doing, for the moment the
    /// thermostat is given control (Monitor to Auto).
    ///
    /// In Monitor mode the thermostat only tracks what it would do, so its
    /// idea of "on" can disagree with the heater, which may have been
    /// switched by hand. Without this, a heater started by hand and then
    /// handed to Auto with the cabin inside the deadband was switched off at
    /// once, because the deadband keeps the thermostat's own (off) state.
    ///
    /// A change of state clears the timer, so the minimum on and off times
    /// are not counted from a switch the thermostat never made. The heater's
    /// own glow plug protection still applies: the hub counts those times
    /// from the last command actually sent (`actuator::should_send`).
    pub fn adopt(&mut self, heat: bool) {
        if self.heat != heat {
            self.heat = heat;
            self.changed_at = None;
        }
    }

    /// Feeds in the latest sample, if any, and returns what the heater should
    /// be doing at `now`. Call this regularly, even without a new sample, so
    /// a lost sensor is noticed.
    pub fn update(&mut self, now: Instant, sample: Option<Sample>) -> Decision {
        let fresh = sample
            .filter(|s| now.saturating_duration_since(s.taken_at) <= self.config.sensor_timeout);

        let Some(sample) = fresh else {
            // Without a temperature there is no basis for burning fuel. This
            // overrides the minimum on time. The minimum off time still
            // applies afterwards, so a flaky sensor cannot cause rapid
            // restarts.
            self.set_heat(false, now);
            return self.decision(Reason::SensorLost);
        };

        let low = self.config.setpoint_c - self.config.hysteresis_c;
        let high = self.config.setpoint_c + self.config.hysteresis_c;
        let since_change = self.changed_at.map(|at| now.saturating_duration_since(at));

        let reason = if sample.temperature_c <= low {
            if self.heat {
                Reason::TooCold
            } else if since_change.is_some_and(|d| d < self.config.min_off) {
                Reason::HoldingMinOff
            } else {
                self.set_heat(true, now);
                Reason::TooCold
            }
        } else if sample.temperature_c >= high {
            if !self.heat {
                Reason::WarmEnough
            } else if since_change.is_some_and(|d| d < self.config.min_on) {
                Reason::HoldingMinOn
            } else {
                self.set_heat(false, now);
                Reason::WarmEnough
            }
        } else {
            Reason::InDeadband
        };

        self.decision(reason)
    }

    fn set_heat(&mut self, heat: bool, now: Instant) {
        if self.heat != heat {
            self.heat = heat;
            self.changed_at = Some(now);
        }
    }

    fn decision(&self, reason: Reason) -> Decision {
        Decision {
            heat: self.heat,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    fn config() -> Config {
        Config {
            setpoint_c: 18.0,
            hysteresis_c: 1.0,
            min_on: 10 * MIN,
            min_off: 10 * MIN,
            sensor_timeout: 2 * MIN,
        }
    }

    /// Updates with a sample taken at `now`.
    fn feed(t: &mut Thermostat, now: Instant, temperature_c: f32) -> Decision {
        t.update(
            now,
            Some(Sample {
                temperature_c,
                taken_at: now,
            }),
        )
    }

    #[test]
    fn starts_off_and_stays_off_when_warm() {
        let mut t = Thermostat::new(config());
        let d = feed(&mut t, Instant::now(), 21.0);
        assert_eq!(
            d,
            Decision {
                heat: false,
                reason: Reason::WarmEnough
            }
        );
    }

    #[test]
    fn first_call_for_heat_is_immediate() {
        let mut t = Thermostat::new(config());
        let d = feed(&mut t, Instant::now(), 17.0);
        assert_eq!(
            d,
            Decision {
                heat: true,
                reason: Reason::TooCold
            }
        );
    }

    #[test]
    fn deadband_keeps_the_current_state() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();

        // Off, and 17.5 is inside the band: stay off.
        assert!(!feed(&mut t, t0, 17.5).heat);

        // Turn on, then drift up through the band: stay on.
        assert!(feed(&mut t, t0 + MIN, 17.0).heat);
        let d = feed(&mut t, t0 + 20 * MIN, 18.9);
        assert_eq!(
            d,
            Decision {
                heat: true,
                reason: Reason::InDeadband
            }
        );
    }

    #[test]
    fn minimum_on_time_is_respected() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(feed(&mut t, t0, 16.0).heat);

        let d = feed(&mut t, t0 + 5 * MIN, 19.5);
        assert_eq!(
            d,
            Decision {
                heat: true,
                reason: Reason::HoldingMinOn
            }
        );

        let d = feed(&mut t, t0 + 10 * MIN, 19.5);
        assert_eq!(
            d,
            Decision {
                heat: false,
                reason: Reason::WarmEnough
            }
        );
    }

    #[test]
    fn minimum_off_time_is_respected() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(feed(&mut t, t0, 16.0).heat);
        assert!(!feed(&mut t, t0 + 10 * MIN, 19.5).heat);

        let d = feed(&mut t, t0 + 15 * MIN, 16.0);
        assert_eq!(
            d,
            Decision {
                heat: false,
                reason: Reason::HoldingMinOff
            }
        );

        let d = feed(&mut t, t0 + 20 * MIN, 16.0);
        assert_eq!(
            d,
            Decision {
                heat: true,
                reason: Reason::TooCold
            }
        );
    }

    #[test]
    fn lost_sensor_turns_heat_off_even_inside_minimum_on() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(feed(&mut t, t0, 16.0).heat);

        let d = t.update(t0 + MIN, None);
        assert_eq!(
            d,
            Decision {
                heat: false,
                reason: Reason::SensorLost
            }
        );
    }

    #[test]
    fn stale_sample_counts_as_lost() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        let sample = Sample {
            temperature_c: 16.0,
            taken_at: t0,
        };

        assert!(t.update(t0 + MIN, Some(sample)).heat);
        let d = t.update(t0 + 3 * MIN, Some(sample));
        assert_eq!(
            d,
            Decision {
                heat: false,
                reason: Reason::SensorLost
            }
        );
    }

    #[test]
    fn sensor_recovery_still_waits_out_minimum_off() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(feed(&mut t, t0, 16.0).heat);
        assert!(!t.update(t0 + MIN, None).heat);

        let d = feed(&mut t, t0 + 2 * MIN, 16.0);
        assert_eq!(
            d,
            Decision {
                heat: false,
                reason: Reason::HoldingMinOff
            }
        );
        assert!(feed(&mut t, t0 + 11 * MIN, 16.0).heat);
    }

    #[test]
    fn lost_sensor_while_off_does_not_restart_the_off_timer() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(feed(&mut t, t0, 16.0).heat);
        assert!(!feed(&mut t, t0 + 10 * MIN, 19.5).heat);

        // Sensor drops out for a while with heat already off.
        assert!(!t.update(t0 + 15 * MIN, None).heat);
        assert!(!t.update(t0 + 19 * MIN, None).heat);

        // Ten minutes after the real switch-off, heat may start again.
        assert!(feed(&mut t, t0 + 20 * MIN, 16.0).heat);
    }

    #[test]
    fn adopting_a_running_heater_keeps_it_on_in_the_deadband() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        // Monitor: the thermostat thinks heat is off.
        assert!(!feed(&mut t, t0, 18.5).heat);
        // The heater was started by hand; Auto takes over inside the band.
        t.adopt(true);
        let d = feed(&mut t, t0 + MIN, 18.5);
        assert_eq!(
            d,
            Decision {
                heat: true,
                reason: Reason::InDeadband
            }
        );
        // Warm enough: stops without waiting for a minimum on time it never
        // started.
        assert!(!feed(&mut t, t0 + 2 * MIN, 19.5).heat);
    }

    #[test]
    fn adopting_the_same_state_keeps_the_timers() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(feed(&mut t, t0, 16.0).heat);
        t.adopt(true);
        assert_eq!(feed(&mut t, t0 + MIN, 19.5).reason, Reason::HoldingMinOn);
    }

    #[test]
    fn setpoint_change_takes_effect() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(!feed(&mut t, t0, 19.5).heat);

        t.set_setpoint(22.0);
        assert!(feed(&mut t, t0 + MIN, 19.5).heat);
    }

    #[test]
    fn a_narrower_band_switches_sooner() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        // ±1: 17.5 is inside the band, so heat stays off.
        assert_eq!(feed(&mut t, t0, 17.5).reason, Reason::InDeadband);
        t.set_hysteresis(0.5);
        assert!(feed(&mut t, t0 + MIN, 17.5).heat);
    }

    #[test]
    fn a_wider_band_holds_longer() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        t.set_hysteresis(2.0);
        // 17.0 would start the heater at ±1, but not at ±2.
        assert_eq!(feed(&mut t, t0, 17.0).reason, Reason::InDeadband);
        assert!(feed(&mut t, t0 + MIN, 16.0).heat);
        // Keeps burning to 20.0, not 19.0.
        assert_eq!(feed(&mut t, t0 + 20 * MIN, 19.5).reason, Reason::InDeadband);
        assert!(!feed(&mut t, t0 + 21 * MIN, 20.0).heat);
    }

    #[test]
    fn narrowing_the_band_keeps_the_minimum_run() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(feed(&mut t, t0, 16.0).heat);
        t.set_hysteresis(0.5);
        // Above the new top (18.5) after 2 minutes: still finishes the run.
        assert_eq!(
            feed(&mut t, t0 + 2 * MIN, 18.6).reason,
            Reason::HoldingMinOn
        );
        assert!(!feed(&mut t, t0 + 10 * MIN, 18.6).heat);
    }

    #[test]
    fn band_parsing_accepts_only_the_dashboard_steps() {
        assert_eq!(parse_band("0.5"), Some(0.5));
        assert_eq!(parse_band(" 1.5 "), Some(1.5));
        assert_eq!(parse_band("3"), Some(3.0));
        assert_eq!(parse_band("2.0000001"), Some(2.0));
        assert_eq!(parse_band("0"), None);
        assert_eq!(parse_band("0.4"), None);
        assert_eq!(parse_band("0.25"), None);
        assert_eq!(parse_band("1.2"), None);
        assert_eq!(parse_band("3.5"), None);
        assert_eq!(parse_band("-1"), None);
        assert_eq!(parse_band("NaN"), None);
        assert_eq!(parse_band("inf"), None);
        assert_eq!(parse_band("wide"), None);
    }

    #[test]
    fn saved_limits_match_the_dashboard_limits() {
        assert_eq!((BAND_MIN_C, BAND_MAX_C, BAND_STEP_C), (0.5, 3.0, 0.5));
    }

    #[test]
    fn saved_round_trip() {
        for band in [0.5, 1.0, 1.5, 2.5, 3.0] {
            let c = Config {
                hysteresis_c: band,
                ..config()
            };
            let saved = Saved::new(&c);
            let (back, seq) = Saved::decode(&saved.encode(9)).unwrap();
            assert_eq!((back, seq), (saved, 9));
            assert_eq!(back.apply(Config::default()).hysteresis_c, band);
        }
    }

    #[test]
    fn saved_default_is_the_default_band() {
        assert_eq!(
            Saved::default().hysteresis_c(),
            Config::default().hysteresis_c
        );
        let bytes = Saved::default().encode(0);
        assert_eq!(Saved::decode(&bytes), Some((Saved::default(), 0)));
    }

    #[test]
    fn saved_apply_keeps_the_other_settings() {
        let saved = Saved::new(&Config {
            hysteresis_c: 2.5,
            ..config()
        });
        let c = saved.apply(Config::default());
        assert_eq!(
            c,
            Config {
                hysteresis_c: 2.5,
                ..Config::default()
            }
        );
    }

    #[test]
    fn saved_rejects_damage_versions_and_bad_bands() {
        let good = Saved::new(&config()).encode(1);

        let mut damaged = good;
        damaged[1] ^= 1;
        assert_eq!(Saved::decode(&damaged), None);

        let mut version = good;
        version[0] = 2;
        assert_eq!(Saved::decode(&version), None);
        assert_eq!(Saved::decode(&good[..8]), None);

        // A good checksum over bands the dashboard refuses.
        for band_x10 in [0, 4, 12, 35, 255] {
            let mut bytes = good;
            bytes[1] = band_x10;
            let sum = fnv1a(&bytes[..8]);
            bytes[8..12].copy_from_slice(&sum.to_le_bytes());
            assert_eq!(Saved::decode(&bytes), None, "band_x10 {band_x10}");
        }
    }
}
