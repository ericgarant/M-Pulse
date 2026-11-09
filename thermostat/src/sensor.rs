//! DHT11 temperature and humidity sampling.
//!
//! The sensor runs on its own thread and publishes the most recent good
//! reading into a [`SharedReading`], so the rest of the firmware never blocks
//! on the sensor's slow single-wire protocol.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dht_sensor::dht11;
use embedded_hal::delay::DelayNs;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{InputOutput, InputPin, OutputPin, PinDriver, Pull};
use log::{info, warn};

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

/// One good measurement from the sensor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// Temperature in degrees Celsius. The DHT11 resolves whole degrees.
    pub temperature_c: f32,
    /// Relative humidity in percent.
    pub humidity_pct: f32,
    /// When the measurement was taken, so consumers can reject stale data.
    pub taken_at: Instant,
}

impl Reading {
    /// Age of this reading.
    pub fn age(&self) -> Duration {
        self.taken_at.elapsed()
    }
}

/// The latest good reading, shared between the sensor thread and consumers.
/// `None` until the first successful read.
pub type SharedReading = Arc<Mutex<Option<Reading>>>;

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

/// Starts the sensor thread on `pin` and returns the shared reading slot.
///
/// The pin is driven open-drain with the internal pull-up enabled. A bare
/// 4-pin DHT11 still needs an external 4.7k to 10k pull-up from DATA to 3.3V,
/// because the internal one is too weak for reliable timing.
pub fn spawn<P>(pin: P) -> anyhow::Result<SharedReading>
where
    P: InputPin + OutputPin + 'static,
{
    let mut pin = PinDriver::input_output_od(pin, Pull::Up)?;
    // Idle state of the bus is high (released).
    pin.set_high()?;

    let shared: SharedReading = Arc::new(Mutex::new(None));
    let publisher = shared.clone();

    thread::Builder::new()
        .name("dht11".into())
        .stack_size(6 * 1024)
        .spawn(move || sample_loop(pin, publisher))?;

    Ok(shared)
}

fn sample_loop(mut pin: PinDriver<'static, InputOutput>, shared: SharedReading) {
    thread::sleep(POWER_UP_DELAY);

    loop {
        match read_with_retries(&mut pin) {
            Some(reading) => {
                info!(
                    "DHT11: {:.0} °C, {:.0} % RH",
                    reading.temperature_c, reading.humidity_pct
                );
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = Some(reading);
            }
            // Keep the previous value in place. Consumers decide when it is
            // too old to trust by checking `Reading::age`.
            None => warn!("DHT11: no valid reading after {READ_ATTEMPTS} attempts"),
        }

        thread::sleep(SAMPLE_INTERVAL);
    }
}

fn read_with_retries(pin: &mut PinDriver<'static, InputOutput>) -> Option<Reading> {
    for attempt in 1..=READ_ATTEMPTS {
        match dht11::blocking::read(&mut DhtDelay, pin) {
            Ok(raw) => {
                return Some(Reading {
                    temperature_c: f32::from(raw.temperature),
                    humidity_pct: f32::from(raw.relative_humidity),
                    taken_at: Instant::now(),
                });
            }
            Err(err) => {
                warn!("DHT11: read attempt {attempt}/{READ_ATTEMPTS} failed: {err:?}");
                // Release the bus and give the sensor time to recover.
                let _ = pin.set_high();
                thread::sleep(RETRY_DELAY);
            }
        }
    }
    None
}
