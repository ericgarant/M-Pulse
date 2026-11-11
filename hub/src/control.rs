//! Thermostat control loop.
//!
//! Runs on its own thread: takes the latest cabin reading from the sensor
//! node, smooths it, and asks the thermostat state machine whether the heater
//! should be running. The decision is published for the dashboard.
//!
//! In Monitor mode that is all it does. In Auto mode it also queues the
//! switch command that moves the heater toward the decision.

use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::actuator::{self, Switch};
use m_pulse_core::filter::MedianFilter;
use m_pulse_core::protocol::RunState;
use m_pulse_core::thermostat::{Decision, Reason, Sample, Thermostat};

use crate::state::{Mode, Shared};

/// How often the thermostat is evaluated. Matches the sensor's sample rate.
const TICK: Duration = Duration::from_secs(5);

/// Readings in the median window. Five samples at one per five seconds
/// reacts within about 15 seconds while ignoring a stray bad reading.
const FILTER_WINDOW: usize = 5;

/// A heater status older than this is not acted on.
const STATUS_MAX_AGE: Duration = Duration::from_secs(10);

/// Starts the control thread.
pub fn spawn(state: Shared) -> anyhow::Result<()> {
    thread::Builder::new()
        .name("thermostat".into())
        .stack_size(6 * 1024)
        .spawn(move || control_loop(state))?;
    Ok(())
}

fn control_loop(state: Shared) {
    let config = state.lock().control.config;
    info!(
        "Thermostat: setpoint {:.1} °C, on at or below {:.1} °C, off at or above {:.1} °C",
        config.setpoint_c,
        config.setpoint_c - config.hysteresis_c,
        config.setpoint_c + config.hysteresis_c,
    );

    let mut thermostat = Thermostat::new(config);
    let mut filter = MedianFilter::<FILTER_WINDOW>::new();
    let mut last_received: Option<Instant> = None;
    let mut smoothed: Option<Sample> = None;
    let mut last_decision: Option<Decision> = None;

    loop {
        let (cabin, setpoint) = {
            let s = state.lock();
            (s.cabin, s.control.config.setpoint_c)
        };

        // The dashboard changes the setpoint in the shared state.
        if (setpoint - thermostat.config().setpoint_c).abs() > 0.01 {
            thermostat.set_setpoint(setpoint);
            info!("Thermostat: setpoint now {setpoint:.1} °C");
        }

        // Feed each new reading through the filter exactly once.
        if let Some(c) = cabin {
            if last_received != Some(c.received_at) {
                last_received = Some(c.received_at);
                smoothed = Some(Sample {
                    temperature_c: filter.push(c.temperature_c),
                    taken_at: c.received_at,
                });
            }
        }

        let decision = thermostat.update(Instant::now(), smoothed);

        if decision.reason == Reason::SensorLost {
            // Old samples must not colour the readings after a dropout.
            filter.clear();
            smoothed = None;
        }

        {
            let mut s = state.lock();
            s.control.decision = Some(decision);
            s.control.smoothed_c = smoothed.map(|x| x.temperature_c);
            if s.control.mode == Mode::Auto {
                drive_heater(&mut s, decision.heat);
            }
        }

        if last_decision != Some(decision) {
            report(&decision, smoothed);
            last_decision = Some(decision);
        }

        thread::sleep(TICK);
    }
}

/// Queues the switch command that moves the heater toward `want_heat`, if
/// the heater is connected, its status is fresh and a command is due.
fn drive_heater(hub: &mut crate::state::Hub, want_heat: bool) {
    let (min_on, min_off) = (hub.control.config.min_on, hub.control.config.min_off);
    let h = &mut hub.heater;
    let (Some(status), Some(at)) = (h.status, h.status_at) else {
        return;
    };
    if !h.connected || at.elapsed() > STATUS_MAX_AGE || h.pending.is_some() {
        return;
    }
    if status.state == RunState::On {
        // The start succeeded, so the next one gets a fresh set of tries.
        // Starting does not count: an ignition can fail and fall back to
        // Off without reporting a fault.
        h.start_attempts = 0;
    }
    let Some(next) = actuator::command(want_heat, &status) else {
        return;
    };
    let last = h.last_sent.map(|(sent, when)| (sent, when.elapsed()));
    if !actuator::should_send(next, last, min_on, min_off) {
        return;
    }
    if next == Switch::On {
        if h.start_attempts >= actuator::MAX_START_ATTEMPTS {
            return;
        }
        h.start_attempts += 1;
        if h.start_attempts == actuator::MAX_START_ATTEMPTS {
            warn!("Thermostat: last start attempt; giving up if the heater stays off");
        }
    }
    info!(
        "Thermostat: heater is {:?}, queueing {next:?}",
        status.state
    );
    h.pending = Some(next);
}

fn report(decision: &Decision, sample: Option<Sample>) {
    let heat = if decision.heat { "ON" } else { "OFF" };
    match sample {
        Some(s) => info!(
            "Thermostat: heat {heat}, {:?}, cabin {:.1} °C",
            decision.reason, s.temperature_c
        ),
        None => warn!(
            "Thermostat: heat {heat}, {:?}, no cabin reading",
            decision.reason
        ),
    }
}
