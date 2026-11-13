//! The hub's own DHT11 cabin sensor.
//!
//! Samples the sensor on its own thread and writes each good reading into the
//! shared state as the cabin reading. While these readings are fresh the hub
//! ignores the Wi-Fi sensor node (see `cabin`); if this sensor fails or is not
//! wired, the node's readings take over.
//!
//! The sampling code is the sensor node's, moved here.

use std::thread;
use std::time::{Duration, Instant};

use dht_sensor::dht11;
use embedded_hal::delay::DelayNs;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{InputOutput, InputPin, OutputPin, PinDriver, Pull};
use log::{info, warn};

use crate::state::{Cabin, Shared, Source};

/// How often the sensor is sampled. The DHT11 must not be polled faster than
/// once per second, and cabin temperature changes slowly anyway.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// The DHT11 needs about a second after power-up before it answers.
const POWER_UP_DELAY: Duration = Duration::from_secs(2);

/// Attempts per sample. A read fails with a timeout or checksum error if an
/// interrupt (for example from the radio) lands in the middle of a bit.
const READ_ATTEMPTS: u32 = 3;

/// Pause between failed attempts within one sample.
const RETRY_DELAY: Duration = Duration::from_millis(1200);

/// Consecutive failed samples before the failure is logged again, so a
/// sensor that is not wired does not fill the log.
const QUIET_FAILURES: u32 = 12;

/// Delay provider for the DHT driver.
///
/// Millisecond delays (the 18 ms start pulse) yield to FreeRTOS so other tasks
/// keep running. Microsecond delays busy-wait, because the bit timing is far
/// shorter than one scheduler tick.
struct DhtDelay;

impl DelayNs for DhtDelay {
    fn delay_ns(&mut self, ns: u32) {
        Ets::delay_us(ns.div_ceil(1_000));
    }

    fn delay_us(&mut self, us: u32) {
        Ets::delay_us(us);
    }

    fn delay_ms(&mut self, ms: u32) {
        FreeRtos::delay_ms(ms);
    }
}

/// Starts the sensor thread on `pin`.
///
/// The pin is driven open-drain with the internal pull-up enabled as well as
/// the external 10k pull-up from DATA to 3.3V.
pub fn spawn<P>(pin: P, state: Shared) -> anyhow::Result<()>
where
    P: InputPin + OutputPin + 'static,
{
    let mut pin = PinDriver::input_output_od(pin, Pull::Up)?;
    // Idle state of the bus is high (released).
    pin.set_high()?;

    thread::Builder::new()
        .name("dht11".into())
        .stack_size(6 * 1024)
        .spawn(move || sample_loop(pin, state))?;
    Ok(())
}

fn sample_loop(mut pin: PinDriver<'static, InputOutput>, state: Shared) {
    thread::sleep(POWER_UP_DELAY);

    let mut failures: u32 = 0;
    loop {
        match read_with_retries(&mut pin) {
            Some((temperature_c, humidity_pct)) => {
                if failures > 0 {
                    info!("DHT11: reading again after {failures} failed samples");
                }
                failures = 0;
                let previous = state.lock().cabin.replace(Cabin {
                    temperature_c,
                    humidity_pct,
                    source: Source::Local,
                    received_at: Instant::now(),
                });
                // Log changes only; the value is usually the same every 5 s.
                let changed = previous.is_none_or(|p| {
                    p.source != Source::Local
                        || p.temperature_c != temperature_c
                        || p.humidity_pct != humidity_pct
                });
                if changed {
                    info!("Cabin: {temperature_c:.0} °C, {humidity_pct:.0} % RH (hub DHT11)");
                }
            }
            // Keep the previous value in place. The thermostat decides when it
            // is too old to trust, and the sensor node can take over.
            None => {
                if failures % QUIET_FAILURES == 0 {
                    warn!("DHT11: no valid reading after {READ_ATTEMPTS} attempts");
                }
                failures = failures.saturating_add(1);
            }
        }

        thread::sleep(SAMPLE_INTERVAL);
    }
}

fn read_with_retries(pin: &mut PinDriver<'static, InputOutput>) -> Option<(f32, f32)> {
    for attempt in 1..=READ_ATTEMPTS {
        match dht11::blocking::read(&mut DhtDelay, pin) {
            Ok(raw) => {
                return Some((
                    f32::from(raw.temperature),
                    f32::from(raw.relative_humidity),
                ));
            }
            Err(err) => {
                log::debug!("DHT11: read attempt {attempt}/{READ_ATTEMPTS} failed: {err:?}");
                // Release the bus and give the sensor time to recover.
                let _ = pin.set_high();
                thread::sleep(RETRY_DELAY);
            }
        }
    }
    None
}
