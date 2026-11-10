//! Thermostat control loop.
//!
//! Runs on its own thread: takes the latest cabin reading from the sensor
//! node, smooths it, and asks the thermostat state machine whether the heater
//! should be running. The decision is published for the dashboard.
//!
//! This is a dry run for now. Nothing is sent to the heater.

use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::filter::MedianFilter;
use m_pulse_core::thermostat::{Decision, Reason, Sample, Thermostat};

use crate::state::Shared;

/// How often the thermostat is evaluated. Matches the sensor's sample rate.
const TICK: Duration = Duration::from_secs(5);

/// Readings in the median window. Five samples at one per five seconds
/// reacts within about 15 seconds while ignoring a stray bad reading.
const FILTER_WINDOW: usize = 5;

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
        "Thermostat (dry run): setpoint {:.1} °C, on at or below {:.1} °C, off at or above {:.1} °C",
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
            info!("Thermostat (dry run): setpoint now {setpoint:.1} °C");
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
        }

        if last_decision != Some(decision) {
            report(&decision, smoothed);
            last_decision = Some(decision);
        }

        thread::sleep(TICK);
    }
}

fn report(decision: &Decision, sample: Option<Sample>) {
    let heat = if decision.heat { "ON" } else { "OFF" };
    match sample {
        Some(s) => info!(
            "Thermostat (dry run): heat {heat}, {:?}, cabin {:.1} °C",
            decision.reason, s.temperature_c
        ),
        None => warn!(
            "Thermostat (dry run): heat {heat}, {:?}, no cabin reading",
            decision.reason
        ),
    }
}
