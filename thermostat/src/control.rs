//! Thermostat control loop.
//!
//! Runs on its own thread: takes the latest cabin reading, smooths it, and
//! asks the thermostat state machine whether the heater should be running.
//!
//! This is a dry run for now. Decisions are logged and nothing is sent to
//! the heater.

use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::filter::MedianFilter;
use m_pulse_core::thermostat::{Config, Decision, Reason, Sample, Thermostat};

use crate::sensor::SharedReading;

/// How often the thermostat is evaluated. Matches the sensor's sample rate.
const TICK: Duration = Duration::from_secs(5);

/// Readings in the median window. Five samples at one per five seconds
/// reacts within about 15 seconds while ignoring a stray bad reading.
const FILTER_WINDOW: usize = 5;

/// Starts the control thread.
pub fn spawn(reading: SharedReading, config: Config) -> anyhow::Result<()> {
    thread::Builder::new()
        .name("thermostat".into())
        .stack_size(6 * 1024)
        .spawn(move || control_loop(reading, config))?;
    Ok(())
}

fn control_loop(reading: SharedReading, config: Config) {
    info!(
        "Thermostat (dry run): setpoint {:.1} °C, on at or below {:.1} °C, off at or above {:.1} °C",
        config.setpoint_c,
        config.setpoint_c - config.hysteresis_c,
        config.setpoint_c + config.hysteresis_c,
    );

    let mut thermostat = Thermostat::new(config);
    let mut filter = MedianFilter::<FILTER_WINDOW>::new();
    let mut last_taken_at: Option<Instant> = None;
    let mut smoothed: Option<Sample> = None;
    let mut last_decision: Option<Decision> = None;

    loop {
        // Feed each new sensor reading through the filter exactly once.
        let latest = *reading.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = latest {
            if last_taken_at != Some(r.taken_at) {
                last_taken_at = Some(r.taken_at);
                smoothed = Some(Sample {
                    temperature_c: filter.push(r.temperature_c),
                    taken_at: r.taken_at,
                });
            }
        }

        let decision = thermostat.update(Instant::now(), smoothed);

        if decision.reason == Reason::SensorLost {
            // Old samples must not colour the readings after a dropout.
            filter.clear();
            smoothed = None;
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
