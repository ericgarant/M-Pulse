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

/// How long an ignition may take before the heater is treated as stuck and
/// may be switched off anyway. A normal start has a flame within 2 minutes
/// and settles within 5.
pub const IGNITION_LIMIT: Duration = Duration::from_secs(600);

/// Whether the heater is burning steadily: it has a flame and the glow plug
/// is off again. The heater reports the state On within seconds of a start
/// command, minutes before it has a flame, so On alone does not mean this.
pub fn is_running(status: &Status) -> bool {
    status.state == RunState::On && status.flame && !status.glow_plug
}

/// The command that moves the heater toward `want_heat`, or `None` if it is
/// already there or must be left alone for now.
///
/// - Never starts a heater that reports a fault.
/// - Never starts a heater that is still shutting down; the start is sent
///   once it reports Off.
/// - Never stops a heater in the middle of its ignition, which is hard on
///   the glow plug; the stop is sent once it is burning steadily. `stuck`
///   lifts this for an ignition that has run past [`IGNITION_LIMIT`].
pub fn command(want_heat: bool, status: &Status, stuck: bool) -> Option<Switch> {
    match (want_heat, status.state) {
        (true, RunState::Off) if status.error.is_none() => Some(Switch::On),
        (false, RunState::On) if is_running(status) || stuck => Some(Switch::Off),
        (false, RunState::Starting) if stuck => Some(Switch::Off),
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

    /// State On with the given flame and glow plug bits.
    fn on(flame: bool, glow_plug: bool) -> Status {
        Status {
            flame,
            glow_plug,
            ..status(RunState::On, None)
        }
    }

    #[test]
    fn starts_only_from_off_without_a_fault() {
        let start = |s: &Status| command(true, s, false);
        assert_eq!(start(&status(RunState::Off, None)), Some(Switch::On));
        assert_eq!(start(&status(RunState::Off, Some(3))), None);
        assert_eq!(start(&status(RunState::ShuttingDown, None)), None);
        assert_eq!(start(&status(RunState::Starting, None)), None);
        assert_eq!(start(&on(true, false)), None);
    }

    #[test]
    fn running_needs_a_flame_and_no_glow_plug() {
        assert!(is_running(&on(true, false)));
        // The sequence seen on a real start: On at once, glow plug, flame
        // with the glow plug still on, then the glow plug goes off.
        assert!(!is_running(&on(false, false)));
        assert!(!is_running(&on(false, true)));
        assert!(!is_running(&on(true, true)));
        assert!(!is_running(&status(RunState::Starting, None)));
    }

    #[test]
    fn stops_only_when_burning_steadily() {
        let stop = |s: &Status| command(false, s, false);
        assert_eq!(stop(&on(true, false)), Some(Switch::Off));
        // A fault never blocks switching off.
        let faulty = Status {
            error: Some(3),
            ..on(true, false)
        };
        assert_eq!(stop(&faulty), Some(Switch::Off));
        // Ignition is left to finish first.
        assert_eq!(stop(&on(false, true)), None);
        assert_eq!(stop(&on(true, true)), None);
        assert_eq!(stop(&status(RunState::Starting, None)), None);
        assert_eq!(stop(&status(RunState::Off, None)), None);
        assert_eq!(stop(&status(RunState::ShuttingDown, None)), None);
    }

    #[test]
    fn a_stuck_ignition_can_be_stopped() {
        assert_eq!(command(false, &on(false, true), true), Some(Switch::Off));
        assert_eq!(
            command(false, &status(RunState::Starting, None), true),
            Some(Switch::Off)
        );
        // Stuck never causes a start or a stop of something already off.
        assert_eq!(command(false, &status(RunState::Off, None), true), None);
        assert_eq!(command(true, &on(false, true), true), None);
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
