//! Thermostat control loop.
//!
//! Runs on its own thread: takes the latest cabin reading from the sensor
//! node, smooths it, and asks the thermostat state machine whether the heater
//! should be running. The decision is published for the dashboard.
//!
//! In Monitor mode that is all it does. In Auto mode it also queues the
//! switch command that moves the heater toward the decision.
//!
//! Before any of that, the low-battery cutoff: while the Bluetti is low it
//! sends the heater its normal Off (a clean stop with the full cool-down)
//! and keeps the thermostat from starting it.
//!
//! The same loop runs the fridge saver: Eco while the Bluetti is low, Max
//! again once it has recovered (see `m_pulse_core::eco_guard`).

use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::actuator::{self, Switch};
use m_pulse_core::filter::MedianFilter;
use m_pulse_core::heater_cutoff;
use m_pulse_core::thermostat::{Decision, Reason, Sample, Thermostat};

use crate::state::{FridgeCommand, Mode, Shared};

/// How often the thermostat is evaluated. Matches the sensor's sample rate.
const TICK: Duration = Duration::from_secs(5);

/// Readings in the median window. Five samples at one per five seconds
/// reacts within about 15 seconds while ignoring a stray bad reading.
const FILTER_WINDOW: usize = 5;

/// A heater status older than this is not acted on.
const STATUS_MAX_AGE: Duration = Duration::from_secs(10);
/// A fridge status older than this is not acted on (the fridge link sends a
/// mode change only with a status this fresh anyway).
const FRIDGE_MAX_AGE: Duration = Duration::from_secs(10);
/// A Bluetti reading older than this is not acted on. It is read every 5 s.
const BLUETTI_MAX_AGE: Duration = Duration::from_secs(30);

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
    // Steady clock for the fridge saver.
    let boot = Instant::now();

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
            let tripped = guard_heater(&mut s);
            if s.control.mode == Mode::Auto && !tripped {
                drive_heater(&mut s, decision.heat);
            }
            save_fridge(&mut s, boot.elapsed().as_secs());
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
    if actuator::is_running(&status) {
        // The start succeeded, so the next one gets a fresh set of tries.
        // The state On alone does not count: the heater reports it seconds
        // after a start command, and an ignition can still fail and fall
        // back to Off without reporting a fault.
        h.start_attempts = 0;
    }
    let last = h.last_sent.map(|(sent, when)| (sent, when.elapsed()));
    // An ignition we started that is still not burning steadily.
    let stuck = matches!(last, Some((Switch::On, ago)) if ago >= actuator::IGNITION_LIMIT);
    let Some(next) = actuator::command(want_heat, &status, stuck) else {
        return;
    };
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

/// The Bluetti's charge, if a reading under `BLUETTI_MAX_AGE` is in.
fn bluetti_soc(hub: &crate::state::Hub) -> Option<u8> {
    let b = &hub.bluetti;
    let fresh = b.status_at.is_some_and(|t| t.elapsed() <= BLUETTI_MAX_AGE);
    (b.connected && fresh)
        .then(|| b.status.as_ref().map(|s| s.soc_pct))
        .flatten()
}

/// Runs the low-battery cutoff one step: while tripped, queues the heater's
/// Off command if it is burning or starting. Returns whether it is tripped.
fn guard_heater(hub: &mut crate::state::Hub) -> bool {
    let soc = bluetti_soc(hub);
    let was = hub.cutoff.cutoff.tripped;
    let config = hub.cutoff.config;
    let tripped = hub.cutoff.cutoff.update(&config, soc);
    if tripped && !was {
        let at = soc.unwrap_or(0);
        warn!(
            "Heater cutoff: Bluetti at {at} %, stopping the heater until it is back to {} %",
            config.resume_pct
        );
        hub.cutoff.tripped_at = Some((at, Instant::now()));
    } else if was && !tripped {
        info!("Heater cutoff: cleared (Bluetti at {soc:?} %)");
    }
    if !tripped {
        return false;
    }

    let h = &mut hub.heater;
    // A start waiting to be sent is dropped.
    if h.pending == Some(Switch::On) {
        h.pending = None;
    }
    let (Some(status), Some(at)) = (h.status, h.status_at) else {
        return true;
    };
    if !h.connected || at.elapsed() > STATUS_MAX_AGE || !heater_cutoff::needs_stop(&status) {
        return true;
    }
    let sent_off_recently = matches!(
        h.last_sent,
        Some((Switch::Off, when)) if when.elapsed() < heater_cutoff::RESEND_OFF_AFTER
    );
    if h.pending.is_none() && !sent_off_recently {
        warn!(
            "Heater cutoff: heater is {:?}, sending Off for a clean stop",
            status.state
        );
        h.pending = Some(Switch::Off);
    }
    true
}

/// Runs the fridge saver one step and queues the mode change it asks for.
fn save_fridge(hub: &mut crate::state::Hub, now_s: u64) {
    let soc = bluetti_soc(hub);
    let fresh = hub
        .fridge
        .status_at
        .is_some_and(|t| t.elapsed() <= FRIDGE_MAX_AGE);
    let fridge = (hub.fridge.connected && fresh)
        .then(|| hub.fridge.status.map(|s| s.mode))
        .flatten();
    let was_low = hub.saver.guard.low;
    let config = hub.saver.config;
    let Some(mode) = hub.saver.guard.update(&config, soc, fridge, now_s) else {
        if was_low && !hub.saver.guard.low {
            info!("Fridge saver: Bluetti back to {soc:?} %");
        }
        return;
    };
    if hub.fridge.pending.is_some() {
        // A dashboard command goes first; the saver tries again later.
        return;
    }
    info!("Fridge saver: Bluetti at {soc:?} %, switching the fridge to {mode:?}");
    hub.fridge.pending = Some(FridgeCommand::Mode(mode));
    hub.saver.last_action = Some((mode, Instant::now()));
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
