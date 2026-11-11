//! Everything the hub knows, in one place.
//!
//! Each part of the firmware (heater and fridge links, cabin receiver, thermostat,
//! dashboard) runs on its own thread and reads or writes this shared state.
//! Hold the lock only long enough to copy values in or out.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use m_pulse_core::actuator::Switch;
use m_pulse_core::fridge::{self, RunMode};
use m_pulse_core::protocol::Status;
use m_pulse_core::thermostat::{Config, Decision};

/// The latest cabin reading from the sensor node.
#[derive(Clone, Copy, Debug)]
pub struct Cabin {
    pub temperature_c: f32,
    pub humidity_pct: f32,
    /// Packet sequence number from the sensor node. Stored for diagnostics;
    /// nothing reads it yet.
    #[allow(dead_code)]
    pub seq: u32,
    pub received_at: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct Heater {
    pub connected: bool,
    pub status: Option<Status>,
    pub status_at: Option<Instant>,
    pub firmware: Option<String>,
    /// Whether the heater's clock was set during the current connection.
    pub clock_synced: bool,
    /// A switch command waiting to be written by the heater link.
    pub pending: Option<Switch>,
    /// The last switch command written, and when.
    pub last_sent: Option<(Switch, Instant)>,
    /// Start commands the thermostat has sent without the heater reaching
    /// On. Auto stops trying at `actuator::MAX_START_ATTEMPTS`.
    pub start_attempts: u8,
}

/// A change the dashboard asked the fridge for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FridgeCommand {
    Power(bool),
    Mode(RunMode),
    /// Target temperature, in the fridge's own unit.
    SetTemp(i8),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Fridge {
    pub connected: bool,
    pub status: Option<fridge::Status>,
    pub status_at: Option<Instant>,
    /// A command waiting to be written by the fridge link.
    pub pending: Option<FridgeCommand>,
    /// The last command written, and when.
    pub last_sent: Option<(FridgeCommand, Instant)>,
    pub warm: fridge::WarmWatch,
    /// Whether the warm alert is on.
    pub warm_alert: bool,
}

/// Who is allowed to switch the heater.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The hub only watches. The thermostat's decision is shown but not
    /// acted on; the dashboard's On and Off buttons still work. This is the
    /// mode after every restart.
    Monitor,
    /// The thermostat switches the heater.
    Auto,
}

#[derive(Clone, Copy, Debug)]
pub struct Control {
    pub mode: Mode,
    pub config: Config,
    pub decision: Option<Decision>,
    /// The filtered cabin temperature the thermostat last acted on.
    pub smoothed_c: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct Hub {
    pub cabin: Option<Cabin>,
    pub heater: Heater,
    pub fridge: Fridge,
    pub control: Control,
    /// The hub's address on your Wi-Fi network, once joined.
    pub sta_ip: Option<String>,
}

#[derive(Clone)]
pub struct Shared(Arc<Mutex<Hub>>);

impl Shared {
    pub fn new(config: Config) -> Self {
        Self(Arc::new(Mutex::new(Hub {
            cabin: None,
            heater: Heater::default(),
            fridge: Fridge::default(),
            control: Control {
                mode: Mode::Monitor,
                config,
                decision: None,
                smoothed_c: None,
            },
            sta_ip: None,
        })))
    }

    /// Locks the state. A panic on another thread never makes it unusable.
    pub fn lock(&self) -> MutexGuard<'_, Hub> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}
