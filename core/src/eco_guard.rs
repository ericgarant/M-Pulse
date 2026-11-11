//! Puts the fridge in Eco while the power station's battery is low.
//!
//! When the Bluetti's charge falls to `low_pct` or below, the fridge is
//! switched from Max to Eco. When the charge is back to `high_pct` or above,
//! it is switched back to Max, but only if this guard was the one that put it
//! in Eco. The gap between the two levels keeps the fridge from flipping back
//! and forth around one level.
//!
//! The guard acts only when the charge crosses a level. If someone changes
//! the fridge's mode by hand in between, the guard leaves it alone.

use crate::fridge::RunMode;

/// Lowest level the dashboard may set, and the highest Eco level.
pub const MIN_PCT: u8 = 5;
pub const MAX_PCT: u8 = 95;
// The Max level may go up to a full battery (100 %).
/// The high level must be at least this far above the low one.
pub const MIN_GAP: u8 = 5;

/// Seconds between tries while the fridge has not taken a change.
pub const RETRY_AFTER_S: u64 = 30;
/// Tries per change before giving up until the next crossing.
pub const MAX_TRIES: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub enabled: bool,
    /// Switch to Eco at or below this charge.
    pub low_pct: u8,
    /// Switch back to Max at or above this charge.
    pub high_pct: u8,
}

impl Default for Config {
    /// Off after every restart, like the thermostat's Monitor mode.
    fn default() -> Self {
        Self {
            enabled: false,
            low_pct: 30,
            high_pct: 50,
        }
    }
}

impl Config {
    /// Checks a pair of levels from the dashboard.
    pub fn with_levels(self, low_pct: u8, high_pct: u8) -> Option<Self> {
        let ok = (MIN_PCT..=MAX_PCT).contains(&low_pct)
            && high_pct <= 100
            && high_pct >= low_pct.saturating_add(MIN_GAP);
        ok.then_some(Self {
            low_pct,
            high_pct,
            ..self
        })
    }
}

/// What the guard is doing, for the dashboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EcoGuard {
    /// The charge has fallen to the low level and not yet recovered.
    pub low: bool,
    /// This guard put the fridge in Eco, so it restores Max on recovery.
    pub switched: bool,
    /// The change being made, until the fridge reports it.
    target: Option<RunMode>,
    tries: u8,
    last_try_s: u64,
}

impl EcoGuard {
    /// Forgets everything; used when the guard is switched off.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// One step. `soc` is the Bluetti's charge and `fridge` the fridge's
    /// mode, each `None` when not known right now; `now_s` is a steady clock
    /// in seconds. Returns a mode to send to the fridge, if one is due.
    pub fn update(
        &mut self,
        config: &Config,
        soc: Option<u8>,
        fridge: Option<RunMode>,
        now_s: u64,
    ) -> Option<RunMode> {
        if !config.enabled {
            self.reset();
            return None;
        }
        // Without both readings, hold still.
        let (soc, fridge) = (soc?, fridge?);

        if !self.low && soc <= config.low_pct {
            self.low = true;
            self.switched = false;
            self.start(if fridge == RunMode::Max {
                Some(RunMode::Eco)
            } else {
                // Already in Eco (by hand): nothing to restore later.
                None
            });
        } else if self.low && soc >= config.high_pct {
            self.low = false;
            let restore = self.switched && fridge == RunMode::Eco;
            self.switched = false;
            self.start(restore.then_some(RunMode::Max));
        }

        let target = self.target?;
        if fridge == target {
            // Done. Eco set by us is ours to undo.
            self.switched = target == RunMode::Eco;
            self.target = None;
            return None;
        }
        if self.tries >= MAX_TRIES {
            self.target = None;
            return None;
        }
        if self.tries > 0 && now_s.saturating_sub(self.last_try_s) < RETRY_AFTER_S {
            return None;
        }
        self.tries += 1;
        self.last_try_s = now_s;
        Some(target)
    }

    fn start(&mut self, target: Option<RunMode>) {
        self.target = target;
        self.tries = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RunMode::{Eco, Max};

    fn on() -> Config {
        Config {
            enabled: true,
            ..Config::default()
        }
    }

    #[test]
    fn levels_are_checked() {
        let c = Config::default();
        assert_eq!(
            c.with_levels(20, 60).map(|c| (c.low_pct, c.high_pct)),
            Some((20, 60))
        );
        assert!(c.with_levels(30, 32).is_none(), "gap too small");
        assert!(c.with_levels(50, 30).is_none(), "reversed");
        assert!(c.with_levels(2, 50).is_none(), "too low");
        assert!(c.with_levels(96, 100).is_none(), "Eco level too high");
        assert!(c.with_levels(95, 100).is_some(), "Max again only when full");
        assert!(c.with_levels(30, 101).is_none(), "over 100");
    }

    #[test]
    fn off_does_nothing() {
        let mut g = EcoGuard::default();
        assert_eq!(g.update(&Config::default(), Some(10), Some(Max), 0), None);
        assert!(!g.low);
    }

    #[test]
    fn eco_when_low_and_max_when_recovered() {
        let mut g = EcoGuard::default();
        let c = on();
        assert_eq!(g.update(&c, Some(40), Some(Max), 0), None);
        assert_eq!(g.update(&c, Some(30), Some(Max), 5), Some(Eco));
        // The fridge takes it.
        assert_eq!(g.update(&c, Some(29), Some(Eco), 10), None);
        assert!(g.switched);
        // Between the levels: hold.
        assert_eq!(g.update(&c, Some(45), Some(Eco), 20), None);
        assert_eq!(g.update(&c, Some(50), Some(Eco), 30), Some(Max));
        assert_eq!(g.update(&c, Some(51), Some(Max), 35), None);
        assert!(!g.low && !g.switched);
    }

    #[test]
    fn retries_then_gives_up() {
        let mut g = EcoGuard::default();
        let c = on();
        assert_eq!(g.update(&c, Some(20), Some(Max), 0), Some(Eco));
        assert_eq!(g.update(&c, Some(20), Some(Max), 10), None, "too soon");
        assert_eq!(g.update(&c, Some(20), Some(Max), 30), Some(Eco));
        assert_eq!(g.update(&c, Some(20), Some(Max), 60), Some(Eco));
        assert_eq!(g.update(&c, Some(20), Some(Max), 90), None, "gave up");
        assert_eq!(g.update(&c, Some(20), Some(Max), 200), None);
    }

    #[test]
    fn leaves_a_manual_change_alone() {
        let mut g = EcoGuard::default();
        let c = on();
        g.update(&c, Some(20), Some(Max), 0);
        g.update(&c, Some(20), Some(Eco), 5);
        // Someone puts it back to Max while the battery is still low.
        assert_eq!(g.update(&c, Some(20), Some(Max), 10), None);
        assert_eq!(g.update(&c, Some(15), Some(Max), 100), None);
        // Recovered: already Max, nothing to send.
        assert_eq!(g.update(&c, Some(60), Some(Max), 200), None);
    }

    #[test]
    fn does_not_restore_eco_it_did_not_set() {
        let mut g = EcoGuard::default();
        let c = on();
        // Already in Eco when the battery runs low.
        assert_eq!(g.update(&c, Some(20), Some(Eco), 0), None);
        assert_eq!(g.update(&c, Some(60), Some(Eco), 10), None);
    }

    #[test]
    fn holds_without_readings() {
        let mut g = EcoGuard::default();
        let c = on();
        assert_eq!(g.update(&c, None, Some(Max), 0), None);
        assert_eq!(g.update(&c, Some(20), None, 0), None);
        assert!(!g.low);
    }

    #[test]
    fn switching_off_forgets() {
        let mut g = EcoGuard::default();
        g.update(&on(), Some(20), Some(Max), 0);
        g.update(&Config::default(), Some(20), Some(Eco), 5);
        assert_eq!(g, EcoGuard::default());
    }
}
