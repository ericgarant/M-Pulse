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

use std::time::{Duration, Instant};

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

    /// Whether heat is currently called for.
    pub fn heat(&self) -> bool {
        self.heat
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
    fn setpoint_change_takes_effect() {
        let mut t = Thermostat::new(config());
        let t0 = Instant::now();
        assert!(!feed(&mut t, t0, 19.5).heat);

        t.set_setpoint(22.0);
        assert!(feed(&mut t, t0 + MIN, 19.5).heat);
    }
}
