//! Everything the hub knows, in one place.
//!
//! Each part of the firmware (heater, fridge and Bluetti links, cabin receiver, thermostat,
//! dashboard) runs on its own thread and reads or writes this shared state.
//! Hold the lock only long enough to copy values in or out.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use m_pulse_core::actuator::Switch;
use m_pulse_core::bluetti;
use m_pulse_core::eco_guard::{self, EcoGuard};
use m_pulse_core::fridge::{self, RunMode};
use m_pulse_core::heater_cutoff::{self, Cutoff};
use m_pulse_core::protocol::Status;
use m_pulse_core::thermostat::{Config, Decision};

/// Where a cabin reading came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The DHT11 wired to the hub itself.
    Local,
    /// The Wi-Fi sensor node.
    Remote,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Local => "hub",
            Source::Remote => "node",
        }
    }
}

/// The latest cabin reading, from the hub's own sensor or the sensor node.
#[derive(Clone, Copy, Debug)]
pub struct Cabin {
    pub temperature_c: f32,
    pub humidity_pct: f32,
    pub source: Source,
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
    /// A power level, in tenths of a kW, waiting to be written by the heater
    /// link. Only the dashboard's + and - buttons set it.
    pub pending_power: Option<u8>,
    /// The last power level written (tenths of a kW), and when.
    pub last_power_sent: Option<(u8, Instant)>,
    /// The latest status report exactly as received, so the bytes the hub
    /// does not decode yet (altitude, efficiency, the heater's own mode
    /// are thought to be in there) can be compared with the LCD remote.
    pub raw_status: Option<[u8; 20]>,
    /// When the heater was first seen burning steadily in this connection,
    /// for the dashboard's run timer. Cleared when it stops or the link drops.
    pub burning_since: Option<Instant>,
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

/// A dashboard request to switch one of the Bluetti's outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputCommand {
    pub output: bluetti::Output,
    pub on: bool,
}

/// The Bluetti power station.
#[derive(Clone, Debug, Default)]
pub struct Bluetti {
    pub connected: bool,
    /// Whether the encrypted key exchange has finished on this connection.
    pub secure: bool,
    pub status: Option<bluetti::Status>,
    pub status_at: Option<Instant>,
    /// An output switch waiting to be written by the Bluetti link.
    pub pending: Option<OutputCommand>,
    /// The last output switch written, whether the station took it, and when.
    pub last_sent: Option<(OutputCommand, bool, Instant)>,
}

/// The rule that puts the fridge in Eco while the Bluetti is low.
#[derive(Clone, Copy, Debug, Default)]
pub struct FridgeSaver {
    pub config: eco_guard::Config,
    pub guard: EcoGuard,
    /// The last mode the rule asked the fridge for, and when.
    pub last_action: Option<(RunMode, Instant)>,
}

/// The safety that stops the heater while the Bluetti is low.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeaterCutoff {
    pub config: heater_cutoff::Config,
    pub cutoff: Cutoff,
    /// The Bluetti's charge when the cutoff last tripped, and when.
    pub tripped_at: Option<(u8, Instant)>,
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
    pub bluetti: Bluetti,
    pub saver: FridgeSaver,
    pub cutoff: HeaterCutoff,
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
            bluetti: Bluetti::default(),
            saver: FridgeSaver::default(),
            cutoff: HeaterCutoff::default(),
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
