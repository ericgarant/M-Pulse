//! The hub's own DHT11 cabin sensor.
//!
//! Samples the sensor on its own thread and writes each good reading into the
//! shared state as the cabin reading. While these readings are fresh the hub
//! ignores the Wi-Fi sensor node (see `cabin`); if this sensor fails or is not
//! wired, the node's readings take over.
//!
//! ## Timing
//!
//! The DHT11 sends its 40 data bits as high pulses of about 27 µs (a 0) or
//! 70 µs (a 1). With the Bluetooth and Wi-Fi links busy, an interrupt landing
//! in the middle of a bit stretched it and every read failed once the Bluetti
//! link was up. So the sensor thread is pinned to core 1 (the radio stack and
//! its interrupts run on core 0), and interrupts on core 1 are held off for
//! the 5 ms or so that the bits take. The 20 ms start pulse before that is an
//! ordinary sleep.

use std::thread;
use std::time::{Duration, Instant};

use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{InputOutput, InputPin, OutputPin, PinDriver, Pull};
use esp_idf_svc::hal::interrupt::IsrCriticalSection;
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;
use log::{info, warn};

use crate::state::{Cabin, Shared, Source};

/// How often the sensor is sampled. The DHT11 must not be polled faster than
/// once per second, and cabin temperature changes slowly anyway.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// The DHT11 needs about a second after power-up before it answers.
const POWER_UP_DELAY: Duration = Duration::from_secs(2);

/// Attempts per sample.
const READ_ATTEMPTS: u32 = 3;

/// Pause between failed attempts within one sample. The DHT11 needs a second
/// between reads.
const RETRY_DELAY: Duration = Duration::from_millis(1200);

/// Consecutive failed samples before the failure is logged again, so a
/// sensor that is not wired does not fill the log.
const QUIET_FAILURES: u32 = 12;

/// How long the host holds the line low to wake the sensor (at least 18 ms).
const START_PULSE_MS: u32 = 20;

/// Longest any single level lasts in a good frame is about 80 µs.
const LEVEL_TIMEOUT_US: i64 = 120;

/// A high pulse longer than this is a 1 bit (0 ≈ 27 µs, 1 ≈ 70 µs).
const ONE_THRESHOLD_US: i64 = 48;

/// Holds off interrupts on the sensor thread's core while the bits arrive.
static BIT_TIMING: IsrCriticalSection = IsrCriticalSection::new();

/// Why a read failed.
#[derive(Debug)]
enum ReadError {
    /// The line did not change level when expected: no sensor, or a bad wire.
    Timeout(&'static str),
    Checksum,
    Pin,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Timeout(step) => write!(f, "no level change ({step})"),
            ReadError::Checksum => f.write_str("checksum mismatch"),
            ReadError::Pin => f.write_str("could not drive the pin"),
        }
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

    // Pin the thread to core 1, away from the radio (see the module notes),
    // then put the default back for every thread spawned after it.
    ThreadSpawnConfiguration {
        pin_to_core: Some(Core::Core1),
        ..Default::default()
    }
    .set()?;
    let spawned = thread::Builder::new()
        .name("dht11".into())
        .stack_size(6 * 1024)
        .spawn(move || sample_loop(pin, state));
    ThreadSpawnConfiguration::default().set()?;
    spawned?;
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
        match read_frame(pin) {
            Ok([rh, _, temp, _, _]) => return Some((f32::from(temp), f32::from(rh))),
            Err(err) => {
                log::debug!("DHT11: read attempt {attempt}/{READ_ATTEMPTS} failed: {err}");
                // Release the bus and give the sensor time to recover.
                let _ = pin.set_high();
                thread::sleep(RETRY_DELAY);
            }
        }
    }
    None
}

/// One DHT11 transaction: wakes the sensor and reads its 5-byte frame
/// (humidity, humidity decimal, temperature, temperature decimal, checksum).
fn read_frame(pin: &mut PinDriver<'static, InputOutput>) -> Result<[u8; 5], ReadError> {
    pin.set_low().map_err(|_| ReadError::Pin)?;
    FreeRtos::delay_ms(START_PULSE_MS);

    let mut frame = [0u8; 5];
    {
        // From here to the last bit the timing matters, so no interrupts on
        // this core (about 5 ms).
        let _quiet = BIT_TIMING.enter();
        pin.set_high().map_err(|_| ReadError::Pin)?;
        // The sensor answers 20-40 µs later with 80 µs low, then 80 µs high.
        wait_for(pin, false, "answer")?;
        wait_for(pin, true, "answer low")?;
        wait_for(pin, false, "answer high")?;
        for bit in 0..40 {
            // Each bit is 50 µs low, then a high pulse whose length is the bit.
            wait_for(pin, true, "bit low")?;
            let rose = now_us();
            wait_for(pin, false, "bit high")?;
            if now_us() - rose > ONE_THRESHOLD_US {
                frame[bit / 8] |= 0x80 >> (bit % 8);
            }
        }
    }

    let sum = frame[..4].iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    if sum == frame[4] {
        Ok(frame)
    } else {
        Err(ReadError::Checksum)
    }
}

/// Busy-waits until the line is at `high`, for at most `LEVEL_TIMEOUT_US`.
fn wait_for(
    pin: &PinDriver<'static, InputOutput>,
    high: bool,
    step: &'static str,
) -> Result<(), ReadError> {
    let start = now_us();
    while pin.is_high() != high {
        if now_us() - start > LEVEL_TIMEOUT_US {
            return Err(ReadError::Timeout(step));
        }
    }
    Ok(())
}

/// Microseconds since boot, from the hardware timer. Safe to call with
/// interrupts off.
fn now_us() -> i64 {
    // SAFETY: a plain read of the system timer, with no preconditions.
    unsafe { esp_idf_svc::sys::esp_timer_get_time() }
}
