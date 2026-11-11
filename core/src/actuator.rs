//! Turns "the thermostat wants heat" into switch commands for the heater.
//!
//! The heater takes minutes to start and to shut down, and it must finish
//! its shutdown (cool-down) cycle before it is started again. This module
//! compares what is wanted with what the heater reports and says which
//! command, if any, to send.

use core::time::Duration;

use crate::protocol::{RunState, Status};

/// How long to wait before repeating a command the heater has not acted on.
pub const RESEND_AFTER: Duration = Duration::from_secs(60);

/// A switch command for the heater.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    On,
    Off,
}

/// Start attempts before giving up on a heater that stays off.
pub const MAX_START_ATTEMPTS: u8 = 3;

/// The command that moves the heater toward `want_heat`, or `None` if it is
/// already there or must be left alone for now.
///
/// - Never starts a heater that reports a fault.
/// - Never starts a heater that is still shutting down; the start is sent
///   once it reports Off.
/// - Never stops a heater in the middle of its start-up, which is hard on
///   the glow plug; the stop is sent once it reports On.
pub fn command(want_heat: bool, status: &Status) -> Option<Switch> {
    match (want_heat, status.state) {
        (true, RunState::Off) if status.error.is_none() => Some(Switch::On),
        (false, RunState::On) => Some(Switch::Off),
        _ => None,
    }
}

/// Whether `next` should be sent now, given the last command sent (by
/// anyone, including the dashboard's buttons) and how long ago.
///
/// - The same command is repeated only after [`RESEND_AFTER`].
/// - Off follows On only after `min_on`, and On follows Off only after
///   `min_off`, so a heater that was just switched by hand is not switched
///   straight back.
pub fn should_send(
    next: Switch,
    last: Option<(Switch, Duration)>,
    min_on: Duration,
    min_off: Duration,
) -> bool {
    match (last, next) {
        (None, _) => true,
        (Some((sent, ago)), _) if sent == next => ago >= RESEND_AFTER,
        (Some((_, ago)), Switch::Off) => ago >= min_on,
        (Some((_, ago)), Switch::On) => ago >= min_off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: RunState, error: Option<u8>) -> Status {
        Status {
            state,
            fan: false,
            fuel_pump: false,
            glow_plug: false,
            flame: false,
            error,
            live_power_kw: 0.0,
            heater_temp_c: 20,
            set_power_kw: 1.4,
            set_temp_c: 25,
            supply_volts: 12.6,
            schedule_enabled: false,
        }
    }

    #[test]
    fn starts_only_from_off_without_a_fault() {
        assert_eq!(
            command(true, &status(RunState::Off, None)),
            Some(Switch::On)
        );
        assert_eq!(command(true, &status(RunState::Off, Some(3))), None);
        assert_eq!(command(true, &status(RunState::ShuttingDown, None)), None);
        assert_eq!(command(true, &status(RunState::Starting, None)), None);
        assert_eq!(command(true, &status(RunState::On, None)), None);
    }

    #[test]
    fn stops_only_when_running() {
        assert_eq!(
            command(false, &status(RunState::On, None)),
            Some(Switch::Off)
        );
        // A fault never blocks switching off.
        assert_eq!(
            command(false, &status(RunState::On, Some(3))),
            Some(Switch::Off)
        );
        // Start-up is left to finish first.
        assert_eq!(command(false, &status(RunState::Starting, None)), None);
        assert_eq!(command(false, &status(RunState::Off, None)), None);
        assert_eq!(command(false, &status(RunState::ShuttingDown, None)), None);
    }

    #[test]
    fn repeats_a_command_only_after_a_pause() {
        let s = Duration::from_secs;
        let (on, off) = (s(600), s(600));
        assert!(should_send(Switch::On, None, on, off));
        assert!(!should_send(Switch::On, Some((Switch::On, s(5))), on, off));
        assert!(should_send(Switch::On, Some((Switch::On, s(60))), on, off));
    }

    #[test]
    fn respects_minimum_on_and_off_times() {
        let s = Duration::from_secs;
        let (on, off) = (s(600), s(300));
        assert!(!should_send(
            Switch::Off,
            Some((Switch::On, s(599))),
            on,
            off
        ));
        assert!(should_send(
            Switch::Off,
            Some((Switch::On, s(600))),
            on,
            off
        ));
        assert!(!should_send(
            Switch::On,
            Some((Switch::Off, s(299))),
            on,
            off
        ));
        assert!(should_send(
            Switch::On,
            Some((Switch::Off, s(300))),
            on,
            off
        ));
    }
}
