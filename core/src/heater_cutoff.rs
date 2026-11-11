//! Stops the heater when the power station's battery runs low.
//!
//! When the Bluetti's charge falls to `stop_pct` or below, the cutoff trips:
//! the hub sends the heater its normal Off command, so the heater runs its
//! full cool-down (fan on, glow plug cleared) instead of losing power
//! mid-burn, and refuses to start it. It resets once the charge is back to
//! `resume_pct` or above. After that the thermostat may start the heater
//! again if it is in Auto; in Monitor the heater stays off until switched on.
//!
//! Without a Bluetti reading the cutoff keeps its last state: a lost link
//! never trips it, and never clears it.

use std::time::Duration;

use crate::protocol::{RunState, Status};

/// Lowest stop level the dashboard may set, and the highest.
pub const MIN_PCT: u8 = 5;
pub const MAX_PCT: u8 = 50;
/// The resume level must be at least this far above the stop level.
pub const MIN_GAP: u8 = 5;

/// Repeat the Off command this often while the heater still reports On or
/// Starting. The heater shows ShuttingDown as soon as it takes the command.
pub const RESEND_OFF_AFTER: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub enabled: bool,
    /// Stop the heater at or below this charge.
    pub stop_pct: u8,
    /// Allow starts again at or above this charge.
    pub resume_pct: u8,
}

impl Default for Config {
    /// On after every restart: it is a safety.
    fn default() -> Self {
        Self {
            enabled: true,
            stop_pct: 15,
            resume_pct: 25,
        }
    }
}

impl Config {
    /// Checks a pair of levels from the dashboard.
    pub fn with_levels(self, stop_pct: u8, resume_pct: u8) -> Option<Self> {
        let ok = (MIN_PCT..=MAX_PCT).contains(&stop_pct)
            && resume_pct <= 100
            && resume_pct >= stop_pct.saturating_add(MIN_GAP);
        ok.then_some(Self {
            stop_pct,
            resume_pct,
            ..self
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cutoff {
    /// The battery is low: the heater must be off and may not start.
    pub tripped: bool,
}

impl Cutoff {
    /// One step with the Bluetti's charge (`None` when not known right now).
    /// Returns whether the cutoff is tripped.
    pub fn update(&mut self, config: &Config, soc: Option<u8>) -> bool {
        if !config.enabled {
            self.tripped = false;
            return false;
        }
        match soc {
            Some(s) if !self.tripped && s <= config.stop_pct => self.tripped = true,
            Some(s) if self.tripped && s >= config.resume_pct => self.tripped = false,
            _ => {}
        }
        self.tripped
    }
}

/// Whether the heater is burning or about to, so needs the Off command.
/// A heater that is cooling down or off is left alone.
pub fn needs_stop(status: &Status) -> bool {
    matches!(status.state, RunState::On | RunState::Starting)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_low_and_resets_high() {
        let c = Config::default();
        let mut k = Cutoff::default();
        assert!(!k.update(&c, Some(40)));
        assert!(!k.update(&c, Some(16)));
        assert!(k.update(&c, Some(15)));
        assert!(
            k.update(&c, Some(24)),
            "between the levels it stays tripped"
        );
        assert!(!k.update(&c, Some(25)));
        assert!(!k.update(&c, Some(20)), "between the levels it stays clear");
    }

    #[test]
    fn missing_reading_keeps_state() {
        let c = Config::default();
        let mut k = Cutoff::default();
        assert!(!k.update(&c, None));
        k.update(&c, Some(10));
        assert!(k.update(&c, None));
    }

    #[test]
    fn disabled_never_trips() {
        let c = Config {
            enabled: false,
            ..Config::default()
        };
        let mut k = Cutoff { tripped: true };
        assert!(!k.update(&c, Some(5)));
    }

    #[test]
    fn levels_are_checked() {
        let c = Config::default();
        assert!(c.with_levels(10, 20).is_some());
        assert!(c.with_levels(20, 22).is_none(), "gap too small");
        assert!(c.with_levels(3, 20).is_none(), "too low");
        assert!(c.with_levels(60, 80).is_none(), "stop level too high");
        assert!(c.with_levels(20, 101).is_none());
    }
}
