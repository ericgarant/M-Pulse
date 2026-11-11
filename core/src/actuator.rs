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

/// The command that moves the heater toward `want_heat`, or `None` if it is
/// already there or must be left alone for now.
///
/// - Never starts a heater that reports a fault.
/// - Never starts a heater that is still shutting down; the start is sent
///   once it reports Off.
pub fn command(want_heat: bool, status: &Status) -> Option<Switch> {
    match (want_heat, status.state) {
        (true, RunState::Off) if status.error.is_none() => Some(Switch::On),
        (false, RunState::On | RunState::Starting) => Some(Switch::Off),
        _ => None,
    }
}

/// Whether `next` should be sent now, given the last command sent and how
/// long ago. A different command goes out at once; the same one is repeated
/// only after [`RESEND_AFTER`].
pub fn should_send(next: Switch, last: Option<(Switch, Duration)>) -> bool {
    match last {
        Some((sent, ago)) if sent == next => ago >= RESEND_AFTER,
        _ => true,
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
    fn stops_when_running_or_starting() {
        assert_eq!(
            command(false, &status(RunState::On, None)),
            Some(Switch::Off)
        );
        assert_eq!(
            command(false, &status(RunState::Starting, None)),
            Some(Switch::Off)
        );
        // A fault never blocks switching off.
        assert_eq!(
            command(false, &status(RunState::On, Some(3))),
            Some(Switch::Off)
        );
        assert_eq!(command(false, &status(RunState::Off, None)), None);
        assert_eq!(command(false, &status(RunState::ShuttingDown, None)), None);
    }

    #[test]
    fn repeats_a_command_only_after_a_pause() {
        let s = Duration::from_secs;
        assert!(should_send(Switch::On, None));
        assert!(!should_send(Switch::On, Some((Switch::On, s(5)))));
        assert!(should_send(Switch::On, Some((Switch::On, s(60)))));
        assert!(should_send(Switch::Off, Some((Switch::On, s(1)))));
    }
}
