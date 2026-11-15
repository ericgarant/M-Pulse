//! Thermostat control loop.
//!
//! Runs on its own thread: takes the latest cabin reading (the hub's own
//! DHT11, or the sensor node as a fallback), smooths it, and asks the
//! thermostat state machine whether the heater should be running. The
//! decision is published for the dashboard.
//!
//! In Monitor mode that is all it does. In Auto mode it also queues the
//! switch command that moves the heater toward the decision.
//!
//! Before any of that, the low-battery cutoff: while the Bluetti is low it
//! sends the heater its normal Off (a clean stop with the full cool-down)
//! and keeps the thermostat from starting it.
//!
//! The fuel reserve works the same way: the loop keeps the fuel estimate
//! (see `m_pulse_core::fuel`) up to date from the heater's reports, saves it
//! to flash, and once the tank is down to the reserve stops the heater
//! cleanly and keeps it from starting.
//!
//! The same loop runs the fridge saver: Eco while the Bluetti is low, Max
//! again once it has recovered (see `m_pulse_core::eco_guard`), and records
//! one point a minute for the dashboard's trend charts.

use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::actuator::{self, Switch};
use m_pulse_core::filter::MedianFilter;
use m_pulse_core::fuel::{Meter, Saved, Seen};
use m_pulse_core::heater_cutoff;
use m_pulse_core::history;
use m_pulse_core::protocol::RunState;
use m_pulse_core::thermostat::{Decision, Reason, Sample, Thermostat};

use crate::fuel_store::FuelStore;
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
pub fn spawn(state: Shared, fuel_store: Option<FuelStore>) -> anyhow::Result<()> {
    thread::Builder::new()
        .name("thermostat".into())
        // 8 kB: the fuel gauge's flash writes run on this thread too.
        .stack_size(8 * 1024)
        .spawn(move || control_loop(state, fuel_store))?;
    Ok(())
}

fn control_loop(state: Shared, mut fuel_store: Option<FuelStore>) {
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
    let mut last_mode = Mode::Monitor;
    // Steady clock for the fridge saver and the history.
    let boot = Instant::now();
    let mut next_point = boot;
    // The fuel estimate: what the loop has seen of the heater, when it last
    // looked, and the copy last written to flash.
    let mut meter = Meter::default();
    let mut last_step = Instant::now();
    let mut last_saved: Option<Saved> = None;

    loop {
        let (cabin, setpoint, mode, heater_now) = {
            let s = state.lock();
            (
                s.cabin,
                s.control.config.setpoint_c,
                s.control.mode,
                heater_on(&s.heater),
            )
        };

        // Auto takes over from what the heater is doing now, not from what
        // the thermostat tracked while it was only watching.
        if mode == Mode::Auto && last_mode != Mode::Auto {
            if let Some(on) = heater_now {
                thermostat.adopt(on);
                info!(
                    "Thermostat: in charge, heater is {}",
                    if on { "on" } else { "off" }
                );
            }
        }
        last_mode = mode;

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

        let fuel_now = {
            let mut s = state.lock();
            s.control.decision = Some(decision);
            s.control.smoothed_c = smoothed.map(|x| x.temperature_c);
            let elapsed = last_step.elapsed();
            last_step = Instant::now();
            let reserve = guard_fuel(&mut s, &mut meter, elapsed);
            let tripped = guard_heater(&mut s);
            if s.control.mode == Mode::Auto && !tripped && !reserve {
                drive_heater(&mut s, decision.heat);
            }
            save_fridge(&mut s, boot.elapsed().as_secs());
            let fuel_now = Saved {
                config: s.fuel.config,
                gauge: s.fuel.gauge,
            };
            if Instant::now() >= next_point {
                next_point += Duration::from_secs(history::STEP_S);
                let point = trend_point(&s);
                drop(s);
                state.history().push(point);
            }
            fuel_now
        };

        // Written outside the lock: a flash write takes a few milliseconds.
        if let Some(store) = fuel_store.as_mut() {
            if last_saved.is_none_or(|last| fuel_now.worth_saving(&last)) {
                match store.save(&fuel_now) {
                    Ok(()) => last_saved = Some(fuel_now),
                    Err(err) => warn!("Fuel gauge: could not save: {err}"),
                }
            }
        }

        if last_decision != Some(decision) {
            report(&decision, smoothed);
            last_decision = Some(decision);
        }

        thread::sleep(TICK);
    }
}

/// Whether the heater is burning or about to (`Some(true)`), fully off or
/// cooling down (`Some(false)`), or unknown: not connected or no fresh
/// status (`None`).
fn heater_on(h: &crate::state::Heater) -> Option<bool> {
    let fresh = h.connected && h.status_at.is_some_and(|at| at.elapsed() <= STATUS_MAX_AGE);
    let status = h.status.filter(|_| fresh)?;
    Some(matches!(status.state, RunState::On | RunState::Starting))
}

/// This minute, for the dashboard's trend history.
fn trend_point(hub: &crate::state::Hub) -> history::Point {
    let cabin = hub.control.smoothed_c;
    let burning = hub.heater.connected
        && hub
            .heater
            .status
            .is_some_and(|st| actuator::is_running(&st));
    history::Point::new(
        cabin,
        hub.control.config.setpoint_c,
        burning,
        bluetti_soc(hub),
    )
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
    hold_off(&mut hub.heater, "Heater cutoff");
    true
}

/// Keeps the heater off for a safety rule (`rule` names it in the log):
/// drops a start waiting to be sent, and queues the normal Off, so the
/// heater runs its full cool-down, while it is burning or starting.
fn hold_off(h: &mut crate::state::Heater, rule: &str) {
    if h.pending == Some(Switch::On) {
        h.pending = None;
    }
    let (Some(status), Some(at)) = (h.status, h.status_at) else {
        return;
    };
    if !h.connected || at.elapsed() > STATUS_MAX_AGE || !heater_cutoff::needs_stop(&status) {
        return;
    }
    let sent_off_recently = matches!(
        h.last_sent,
        Some((Switch::Off, when)) if when.elapsed() < heater_cutoff::RESEND_OFF_AFTER
    );
    if h.pending.is_none() && !sent_off_recently {
        warn!(
            "{rule}: heater is {:?}, sending Off for a clean stop",
            status.state
        );
        h.pending = Some(Switch::Off);
    }
}

/// Runs the fuel estimate one step over `elapsed`, and the reserve: while
/// the tank is at the reserve, keeps the heater off. Returns whether the
/// reserve holds the heater off.
fn guard_fuel(hub: &mut crate::state::Hub, meter: &mut Meter, elapsed: Duration) -> bool {
    let h = &hub.heater;
    let fresh = h.connected && h.status_at.is_some_and(|at| at.elapsed() <= STATUS_MAX_AGE);
    let seen = Seen::from_status(h.status.as_ref().filter(|_| fresh));

    let config = hub.fuel.config;
    let gauge = &mut hub.fuel.gauge;
    let (was_low, was_uncertain) = (gauge.low(&config), gauge.uncertain);
    meter.step(gauge, seen, elapsed);
    let gauge = *gauge;
    if gauge.uncertain && !was_uncertain {
        warn!("Fuel gauge: some burning may have gone uncounted; the estimate is uncertain until the next fill-up");
    }
    if gauge.low(&config) && !was_low {
        warn!(
            "Fuel gauge: low, about {:.2} L left ({:.0} %)",
            gauge.level_l.unwrap_or(0.0),
            gauge.percent(&config).unwrap_or(0.0)
        );
    }

    if !gauge.at_reserve(&config) {
        if hub.fuel.reserve_since.take().is_some() {
            info!("Fuel reserve: cleared, heater starts allowed again");
        }
        return false;
    }
    if hub.fuel.reserve_since.is_none() {
        warn!(
            "Fuel reserve: tank down to {} %, stopping the heater and blocking starts until it is refilled",
            config.reserve_pct
        );
        hub.fuel.reserve_since = Some(Instant::now());
    }
    hold_off(&mut hub.heater, "Fuel reserve");
    true
}

/// Runs the fridge saver one step and queues the mode change it asks for.
fn save_fridge(hub: &mut crate::state::Hub, now_s: u64) {
    let soc = bluetti_soc(hub);
    let fresh = hub
        .fridge
        .status_at
        .is_some_and(|t| t.elapsed() <= FRIDGE_MAX_AGE);
    // While a command waits for the fridge link, the mode is about to
    // change: the saver holds still rather than spend one of its tries on a
    // change it cannot queue.
    let settled = hub.fridge.pending.is_none();
    let fridge = (hub.fridge.connected && fresh && settled)
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
