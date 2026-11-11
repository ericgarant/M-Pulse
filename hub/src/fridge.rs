//! BougeRV Rocky 50 fridge BLE client.
//!
//! Keeps a lasting link to the fridge: finds it, connects, subscribes to its
//! replies and asks for its status every two seconds (the fridge never
//! reports unasked). Each status is published to the shared state and fed to
//! the warm-fridge alert. If the link drops or the fridge goes quiet, it
//! reconnects.
//!
//! Besides the status query, the only commands written are the ones the
//! dashboard queues: power, Eco/Max and the target temperature. A queued
//! command is dropped, never sent late, if the link goes down first.

use std::time::{Duration, Instant};

use embassy_futures::select::{select, Either};
use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{uuid128, BLEAddress, BLEClient, BLEDevice, BLEError, BLEScan};
use esp_idf_svc::timer::EspAsyncTimer;
use log::{error, info, warn};
use m_pulse_core::fridge::{self, Reassembler, Status};

use crate::radio;
use crate::state::{FridgeCommand, Shared};

/// The fridge advertises as `SYZ-R50-` followed by the end of its address.
const NAME_PREFIX: &str = "SYZ-R50";

const FRIDGE_SERVICE: BleUuid = uuid128!("00001234-0000-1000-8000-00805f9b34fb");
/// Commands are written here (write without response).
const CHAR_WRITE: BleUuid = uuid128!("00001235-0000-1000-8000-00805f9b34fb");
/// Replies arrive here as notifications.
const CHAR_NOTIFY: BleUuid = uuid128!("00001236-0000-1000-8000-00805f9b34fb");

/// How long to look for the fridge in each attempt.
const FIND_DURATION_MS: i32 = 15_000;
/// Give up on a connection attempt after this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Pause before looking for the fridge again after a link ends.
const RETRY_PAUSE: Duration = Duration::from_secs(10);
/// How often to ask for the status. The vendor app asks about as often.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// No answer for this long closes the link and rebuilds it.
const SILENCE_LIMIT: Duration = Duration::from_secs(15);
/// Power and mode changes rewrite the whole settings block from the last
/// status, so that status must be this fresh.
const SETTINGS_MAX_AGE: Duration = Duration::from_secs(10);

/// Keeps the fridge link up for the life of the firmware.
pub async fn run(
    device: &BLEDevice,
    timer: &mut EspAsyncTimer,
    state: Shared,
) -> anyhow::Result<()> {
    // Steady clock for the warm alert.
    let boot = Instant::now();
    loop {
        let turn = radio::take(timer).await?;
        match find(device, FIND_DURATION_MS).await {
            Ok(Some(addr)) => {
                if let Err(err) = session(device, &addr, timer, &state, boot, turn).await {
                    warn!("Fridge link ended: {err}");
                }
            }
            Ok(None) => info!("Fridge not in range"),
            Err(err) => error!("Fridge search failed: {err}"),
        }
        {
            let mut s = state.lock();
            s.fridge.connected = false;
            s.fridge.pending = None;
            // A warm spell cannot be timed without readings.
            s.fridge.warm.reset();
            s.fridge.warm_alert = false;
        }
        timer.after(RETRY_PAUSE).await?;
    }
}

async fn find(device: &BLEDevice, duration_ms: i32) -> Result<Option<BLEAddress>, BLEError> {
    let mut scan = BLEScan::new();
    scan.active_scan(true).interval(100).window(99);
    scan.start(device, duration_ms, |adv, data| {
        let name = data.name()?;
        if !name.to_string().starts_with(NAME_PREFIX) {
            return None;
        }
        info!(
            "Fridge found: {} at {} ({} dBm)",
            name,
            adv.addr(),
            adv.rssi()
        );
        Some(adv.addr())
    })
    .await
}

async fn session(
    device: &BLEDevice,
    addr: &BLEAddress,
    timer: &mut EspAsyncTimer,
    state: &Shared,
    boot: Instant,
    turn: radio::Turn,
) -> anyhow::Result<()> {
    let mut client = device.new_client();
    let on_drop = state.clone();
    client.on_disconnect(move |reason| {
        warn!("Fridge disconnected (reason {reason:#x})");
        on_drop.lock().fridge.connected = false;
    });

    info!("Connecting to fridge at {addr}");
    match select(client.connect(addr), timer.after(CONNECT_TIMEOUT)).await {
        Either::First(connected) => connected?,
        Either::Second(_) => {
            if client.connected() {
                let _ = client.disconnect();
            }
            anyhow::bail!("connection attempt timed out");
        }
    }
    drop(turn);
    info!("Fridge connected, RSSI {:?} dBm", client.get_rssi().ok());
    {
        let mut s = state.lock();
        s.fridge.connected = true;
        s.fridge.status_at = None;
    }

    let started = Instant::now();
    let result = stay_connected(&mut client, timer, state, boot).await;
    if client.connected() {
        let _ = client.disconnect();
    }
    info!("Fridge link lasted {} s", started.elapsed().as_secs());
    result
}

async fn stay_connected(
    client: &mut BLEClient,
    timer: &mut EspAsyncTimer,
    state: &Shared,
    boot: Instant,
) -> anyhow::Result<()> {
    let service = client.get_service(FRIDGE_SERVICE).await?;
    let notify = service.get_characteristic(CHAR_NOTIFY).await?;
    let publish = state.clone();
    let mut assembler = Reassembler::default();
    let mut last: Option<Status> = None;
    let mut raw_logged = 0u8;
    notify.on_notify(move |data| {
        // The first few notifications in full, for diagnosis.
        if raw_logged < 3 {
            raw_logged += 1;
            info!("FRIDGE raw {data:02x?}");
        }
        let Some(frame) = assembler.push(data) else {
            return;
        };
        let Some(status) = Status::parse(&frame) else {
            warn!("FRIDGE (unrecognised) {frame:02x?}");
            return;
        };
        if last != Some(status) {
            info!(
                "Fridge: power {} mode {:?} set {} inside {} input {:.1} V lock {} prot {:?}",
                status.powered,
                status.mode,
                status.set_temp,
                status.temp,
                status.input_volts,
                status.locked,
                status.battery_protection
            );
            last = Some(status);
        }
        let now_s = boot.elapsed().as_secs();
        let mut s = publish.lock();
        let alert = s.fridge.warm.update(now_s, &status);
        if alert && !s.fridge.warm_alert {
            warn!(
                "Fridge warm alert: {} inside, target {}",
                status.temp, status.set_temp
            );
        }
        s.fridge.warm_alert = alert;
        s.fridge.status = Some(status);
        s.fridge.status_at = Some(Instant::now());
    });
    // The fridge ignores a subscription written without a response.
    notify.subscribe_notify(true).await?;
    info!("Subscribed to fridge replies");

    let subscribed_at = Instant::now();
    while client.connected() {
        let queued = state.lock().fridge.pending.take();
        if let Some(command) = queued {
            match command_frame(state, command) {
                Ok(frame) => {
                    warn!("Fridge command {command:?}: writing {frame:02x?}");
                    write(client, &frame).await?;
                    state.lock().fridge.last_sent = Some((command, Instant::now()));
                    // Let the fridge apply it before the next query.
                    timer.after(Duration::from_millis(300)).await?;
                }
                Err(why) => warn!("Fridge command {command:?} not sent: {why}"),
            }
        }

        write(client, &fridge::query_frame()).await?;

        let last_status = state.lock().fridge.status_at.unwrap_or(subscribed_at);
        if last_status.elapsed() > SILENCE_LIMIT {
            anyhow::bail!("no reply for {} s", SILENCE_LIMIT.as_secs());
        }
        timer.after(POLL_EVERY).await?;
    }
    anyhow::bail!("link lost")
}

/// The bytes for a dashboard command, checked against the last status.
fn command_frame(state: &Shared, command: FridgeCommand) -> Result<Vec<u8>, &'static str> {
    let (status, at) = {
        let s = state.lock();
        (s.fridge.status, s.fridge.status_at)
    };
    let (Some(status), Some(at)) = (status, at) else {
        return Err("no status yet");
    };
    match command {
        FridgeCommand::SetTemp(t) => Ok(fridge::set_temp_frame(
            t.clamp(status.min_temp, status.max_temp),
        )),
        _ if at.elapsed() > SETTINGS_MAX_AGE => Err("status too old to rewrite settings"),
        FridgeCommand::Power(on) => Ok(status.power_frame(on)),
        FridgeCommand::Mode(mode) => Ok(status.mode_frame(mode)),
    }
}

async fn write(client: &mut BLEClient, frame: &[u8]) -> anyhow::Result<()> {
    client
        .get_service(FRIDGE_SERVICE)
        .await?
        .get_characteristic(CHAR_WRITE)
        .await?
        .write_value(frame, false)
        .await?;
    Ok(())
}
