//! The hub's own SHT3x cabin sensor, on I2C.
//!
//! Samples the sensor on its own thread and writes each good reading into the
//! shared state as the cabin reading. While these readings are fresh the hub
//! ignores the Wi-Fi sensor node (see `cabin`); if this sensor fails or is not
//! wired, the node's readings take over.
//!
//! The ESP32-S3's I2C controller does the bit timing in hardware, so unlike
//! the DHT11 this replaced, reads need no pinned core and no interrupts held
//! off: the radio cannot corrupt them. Each transfer is still checked by the
//! sensor's CRC (see `m_pulse_core::sht3x`).
//!
//! The sensor answers at 0x44 or 0x45 depending on its ADDR pin. The hub
//! looks for it at both, so the board works whichever way ADDR is wired, and
//! looks again after a run of failed reads (a loose wire, or a sensor plugged
//! in after boot).

use std::thread;
use std::time::{Duration, Instant};

use esp_idf_svc::hal::delay::{FreeRtos, TickType};
use esp_idf_svc::hal::gpio::{InputPin, OutputPin};
use esp_idf_svc::hal::i2c::{I2c, I2cConfig, I2cDriver};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::EspError;
use log::{info, warn};
use m_pulse_core::sht3x;

use crate::state::{Cabin, Shared, Source};

/// How often the sensor is sampled. Cabin temperature changes slowly.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// The SHT3x is ready about 1 ms after power-up; this also lets the rest of
/// the hub start first.
const POWER_UP_DELAY: Duration = Duration::from_secs(2);

/// Attempts per sample.
const READ_ATTEMPTS: u32 = 3;

/// Pause between failed attempts within one sample.
const RETRY_DELAY: Duration = Duration::from_millis(200);

/// Consecutive failed samples before the failure is logged again, so a
/// sensor that is not wired does not fill the log. Also how many failed
/// samples it takes before the hub looks for the sensor again.
const QUIET_FAILURES: u32 = 12;

/// Standard-mode I2C. Fast enough for one 6-byte read every 5 s, and the
/// most forgiving of long wires and weak pull-ups.
const BUS_SPEED: Hertz = Hertz(100_000);

/// Longest a single I2C transfer may take before it counts as failed.
const BUS_TIMEOUT: TickType = TickType::new_millis(50);

/// Why a read failed.
#[derive(Debug)]
enum ReadError {
    /// The sensor did not acknowledge, or the bus timed out: no sensor, a
    /// bad wire, or missing pull-ups.
    Bus(EspError),
    /// The bytes arrived but did not make a good reading.
    Frame(sht3x::FrameError),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Bus(err) => write!(f, "no answer on I2C ({err})"),
            ReadError::Frame(err) => write!(f, "bad frame ({err:?})"),
        }
    }
}

impl From<EspError> for ReadError {
    fn from(err: EspError) -> Self {
        ReadError::Bus(err)
    }
}

/// Starts the sensor thread on the I2C controller `i2c`, with the sensor's
/// SDA and SCL on `sda` and `scl`.
///
/// The ESP32's internal pull-ups are enabled as well, but they are weak
/// (about 45k); the breakout board's own 10k pull-ups do the real work.
pub fn spawn<I, D, C>(i2c: I, sda: D, scl: C, state: Shared) -> anyhow::Result<()>
where
    I: I2c + 'static,
    D: InputPin + OutputPin + 'static,
    C: InputPin + OutputPin + 'static,
{
    let config = I2cConfig::new()
        .baudrate(BUS_SPEED)
        .sda_enable_pullup(true)
        .scl_enable_pullup(true);
    let bus = I2cDriver::new(i2c, sda, scl, &config)?;

    thread::Builder::new()
        .name("sht3x".into())
        .stack_size(6 * 1024)
        .spawn(move || sample_loop(bus, state))?;
    Ok(())
}

fn sample_loop(mut bus: I2cDriver<'static>, state: Shared) {
    thread::sleep(POWER_UP_DELAY);

    let mut address: Option<u8> = None;
    let mut failures: u32 = 0;
    loop {
        if address.is_none() {
            address = find(&mut bus);
            if let Some(addr) = address {
                info!("SHT3x: found at {addr:#04x}");
            }
        }

        match address.and_then(|addr| read_with_retries(&mut bus, addr)) {
            Some((temperature_c, humidity_pct)) => {
                if failures > 0 {
                    info!("SHT3x: reading again after {failures} failed samples");
                }
                failures = 0;
                let previous = state.lock().cabin.replace(Cabin {
                    temperature_c,
                    humidity_pct,
                    source: Source::Local,
                    received_at: Instant::now(),
                });
                // Log changes of a tenth of a degree or a whole percent
                // only. The SHT3x resolves far finer than that, so exact
                // comparison would log every sample.
                let changed = previous.is_none_or(|p| {
                    p.source != Source::Local
                        || (p.temperature_c - temperature_c).abs() >= 0.1
                        || (p.humidity_pct - humidity_pct).abs() >= 1.0
                });
                if changed {
                    info!("Cabin: {temperature_c:.1} °C, {humidity_pct:.0} % RH (hub SHT3x)");
                }
            }
            // Keep the previous value in place. The thermostat decides when it
            // is too old to trust, and the sensor node can take over.
            None => {
                if failures % QUIET_FAILURES == 0 {
                    match address {
                        Some(addr) => warn!(
                            "SHT3x: no valid reading at {addr:#04x} after {READ_ATTEMPTS} attempts"
                        ),
                        None => warn!(
                            "SHT3x: no sensor at {:#04x} or {:#04x}; check SDA, SCL and power",
                            sht3x::ADDR_LOW,
                            sht3x::ADDR_HIGH
                        ),
                    }
                }
                failures = failures.saturating_add(1);
                // After a run of failures, look for the sensor again; a
                // reset on the way also clears a sensor stuck mid-command.
                if failures % QUIET_FAILURES == 0 {
                    address = None;
                }
            }
        }

        thread::sleep(SAMPLE_INTERVAL);
    }
}

/// Looks for the sensor at both addresses by sending it a soft reset, which
/// it acknowledges only at its own address. Returns the address that
/// answered.
fn find(bus: &mut I2cDriver<'static>) -> Option<u8> {
    let addr = sht3x::ADDRESSES.into_iter().find(|&addr| {
        bus.write(addr, &sht3x::CMD_SOFT_RESET, BUS_TIMEOUT.ticks())
            .is_ok()
    })?;
    FreeRtos::delay_ms(sht3x::RESET_TIME_MS);
    Some(addr)
}

fn read_with_retries(bus: &mut I2cDriver<'static>, addr: u8) -> Option<(f32, f32)> {
    for attempt in 1..=READ_ATTEMPTS {
        match read(bus, addr) {
            Ok(reading) => return Some(reading),
            Err(err) => {
                log::debug!("SHT3x: read attempt {attempt}/{READ_ATTEMPTS} failed: {err}");
                thread::sleep(RETRY_DELAY);
            }
        }
    }
    None
}

/// One single-shot measurement: start it, wait for it to finish, then read
/// and decode the 6-byte frame.
fn read(bus: &mut I2cDriver<'static>, addr: u8) -> Result<(f32, f32), ReadError> {
    bus.write(addr, &sht3x::CMD_MEASURE, BUS_TIMEOUT.ticks())?;
    // An ordinary sleep: nothing on the bus is timing-sensitive meanwhile.
    FreeRtos::delay_ms(sht3x::MEASURE_TIME_MS);
    let mut frame = [0u8; 6];
    bus.read(addr, &mut frame, BUS_TIMEOUT.ticks())?;
    sht3x::decode(frame).map_err(ReadError::Frame)
}
