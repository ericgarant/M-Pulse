//! Bluetti Elite 200 V2 power station BLE client.
//!
//! Keeps a lasting link to the power station: finds it, connects, runs the
//! encrypted key exchange the station requires, then reads the app's home
//! block (battery, inputs, outputs) every few seconds and publishes it to the
//! shared state. If the link drops, the exchange fails or the station goes
//! quiet, it reconnects.
//!
//! Besides those reads, the only thing written is an AC or DC output switch
//! the dashboard asked for (see `web`). It is sent once, never retried, and
//! dropped if the link goes down first. No other setting is changed.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use embassy_futures::select::{select, Either};
use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{BLEAddress, BLEClient, BLEDevice, BLEError, BLEScan};
use esp_idf_svc::timer::EspAsyncTimer;
use log::{error, info, warn};
use m_pulse_core::bluetti::{self, Channel, Event, Status, HOME_COUNT, HOME_START};

use crate::radio;
use crate::state::{OutputCommand, Shared};

/// Every Bluetti advertises this service.
const BLUETTI_SERVICE: BleUuid = BleUuid::from_uuid16(0xFF00);
/// Requests are written here.
const CHAR_WRITE: BleUuid = BleUuid::from_uuid16(0xFF02);
/// Replies and key-exchange messages arrive here.
const CHAR_NOTIFY: BleUuid = BleUuid::from_uuid16(0xFF01);

/// How long to look for the station in each attempt.
const FIND_DURATION_MS: i32 = 15_000;
/// Give up on a connection attempt after this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The key exchange must finish within this long of subscribing.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Pause before looking for the station again after a link ends.
const RETRY_PAUSE: Duration = Duration::from_secs(10);
/// How often to read the home block. Battery and power change slowly.
const POLL_EVERY: Duration = Duration::from_secs(5);
/// Wait this long for each reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(4);
/// This many missed replies in a row closes the link and rebuilds it.
const MAX_MISSES: u8 = 4;
/// How often the wait between reads looks for a dashboard command.
const COMMAND_CHECK: Duration = Duration::from_millis(100);

/// Notifications waiting for the link task. The NimBLE callback cannot
/// write, so it only queues; the task answers the key exchange.
type Inbox = Arc<Mutex<VecDeque<Vec<u8>>>>;

/// Keeps the Bluetti link up for the life of the firmware.
pub async fn run(
    device: &BLEDevice,
    timer: &mut EspAsyncTimer,
    state: Shared,
) -> anyhow::Result<()> {
    loop {
        let turn = radio::take(timer).await?;
        match find(device, FIND_DURATION_MS).await {
            Ok(Some(addr)) => {
                if let Err(err) = session(device, &addr, timer, &state, turn).await {
                    warn!("Bluetti link ended: {err}");
                }
            }
            // Hand the radio on before the pause below, or this link takes
            // it straight back and the others never get to search.
            Ok(None) => {
                drop(turn);
                info!("Bluetti not in range");
            }
            Err(err) => {
                drop(turn);
                error!("Bluetti search failed: {err}");
            }
        }
        {
            let mut s = state.lock();
            s.bluetti.connected = false;
            s.bluetti.secure = false;
            // Never send a switch late, after the link has been rebuilt.
            s.bluetti.pending = None;
        }
        timer.after(RETRY_PAUSE).await?;
    }
}

async fn find(device: &BLEDevice, duration_ms: i32) -> Result<Option<BLEAddress>, BLEError> {
    let mut scan = BLEScan::new();
    scan.active_scan(true)
        .interval(radio::SCAN_INTERVAL)
        .window(radio::SCAN_WINDOW);
    scan.start(device, duration_ms, |adv, data| {
        if !data.is_advertising_service(&BLUETTI_SERVICE) {
            return None;
        }
        let name = data.name().map(|n| n.to_string()).unwrap_or_default();
        info!(
            "Bluetti found: {name:?} at {} ({} dBm)",
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
    turn: radio::Turn,
) -> anyhow::Result<()> {
    let mut client = device.new_client();
    let on_drop = state.clone();
    client.on_disconnect(move |reason| {
        warn!("Bluetti disconnected (reason {reason:#x})");
        let mut s = on_drop.lock();
        s.bluetti.connected = false;
        s.bluetti.secure = false;
    });

    info!("Connecting to Bluetti at {addr}");
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
    info!("Bluetti connected, RSSI {:?} dBm", client.get_rssi().ok());
    {
        let mut s = state.lock();
        s.bluetti.connected = true;
        s.bluetti.status_at = None;
    }

    let started = Instant::now();
    let result = stay_connected(&mut client, timer, state).await;
    if client.connected() {
        let _ = client.disconnect();
    }
    info!("Bluetti link lasted {} s", started.elapsed().as_secs());
    result
}

async fn stay_connected(
    client: &mut BLEClient,
    timer: &mut EspAsyncTimer,
    state: &Shared,
) -> anyhow::Result<()> {
    let service = client.get_service(BLUETTI_SERVICE).await?;
    let notify = service.get_characteristic(CHAR_NOTIFY).await?;
    let inbox: Inbox = Arc::default();
    let sink = inbox.clone();
    let mut raw_logged = 0u8;
    notify.on_notify(move |data| {
        // The first few notifications, for diagnosis.
        if raw_logged < 3 {
            raw_logged += 1;
            info!(
                "BLUETTI raw {} bytes {:02x?}",
                data.len(),
                &data[..data.len().min(24)]
            );
        }
        sink.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(data.to_vec());
    });
    notify.subscribe_notify(true).await?;
    info!(
        "Subscribed to Bluetti (MTU {:?}); waiting for its key exchange",
        client.desc().map(|d| d.mtu()).ok()
    );

    let mut channel = Channel::new();
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    while !channel.is_ready() {
        if !client.connected() {
            anyhow::bail!("link lost during key exchange");
        }
        if Instant::now() > deadline {
            anyhow::bail!("key exchange did not finish");
        }
        for event in drain(&inbox, &mut channel) {
            match event {
                Event::Send(bytes) => write(client, &bytes).await?,
                Event::Ready => info!("Bluetti encrypted link ready"),
                Event::Data(_) | Event::Nothing => {}
            }
        }
        timer.after(Duration::from_millis(100)).await?;
    }
    state.lock().bluetti.secure = true;

    let request = bluetti::read_request(HOME_START, HOME_COUNT);
    let mut last: Option<Status> = None;
    let mut misses = 0u8;
    while client.connected() {
        let sealed = channel
            .seal(&request, random())
            .ok_or_else(|| anyhow::anyhow!("channel lost its key"))?;
        write(client, &sealed).await?;

        match wait_reply(&inbox, &mut channel, timer).await? {
            Some(reply) => match bluetti::read_reply(&reply, HOME_COUNT)
                .ok()
                .and_then(Status::parse)
            {
                Some(status) => {
                    misses = 0;
                    if last.as_ref() != Some(&status) {
                        log_change(last.as_ref(), &status);
                        last = Some(status.clone());
                    }
                    let mut s = state.lock();
                    s.bluetti.status = Some(status);
                    s.bluetti.status_at = Some(Instant::now());
                }
                None => {
                    misses += 1;
                    warn!("Bluetti reply not understood: {reply:02x?}");
                }
            },
            None => {
                misses += 1;
                warn!("No Bluetti reply ({misses} in a row)");
            }
        }
        if misses >= MAX_MISSES {
            anyhow::bail!("{MAX_MISSES} replies missed");
        }

        // Wait for the next read, but send a dashboard command as soon as
        // one arrives, then read straight away so the page shows the result.
        let next_read = Instant::now() + POLL_EVERY;
        while client.connected() && Instant::now() < next_read {
            let queued = state.lock().bluetti.pending.take();
            if let Some(command) = queued {
                switch_output(client, &inbox, &mut channel, timer, state, command).await?;
                break;
            }
            timer.after(COMMAND_CHECK).await?;
        }
    }
    anyhow::bail!("link lost")
}

/// Writes one output switch and checks the station's echo.
async fn switch_output(
    client: &mut BLEClient,
    inbox: &Inbox,
    channel: &mut Channel,
    timer: &mut EspAsyncTimer,
    state: &Shared,
    command: OutputCommand,
) -> anyhow::Result<()> {
    let OutputCommand { output, on } = command;
    let request = output.switch_request(on);
    warn!("Bluetti command {command:?}: writing {request:02x?}");
    let sealed = channel
        .seal(&request, random())
        .ok_or_else(|| anyhow::anyhow!("channel lost its key"))?;
    write(client, &sealed).await?;
    let taken = match wait_reply(inbox, channel, timer).await? {
        Some(reply) => match output.switch_reply(&reply, on) {
            Ok(()) => {
                info!("Bluetti took {command:?}");
                true
            }
            Err(err) => {
                warn!("Bluetti refused {command:?} ({err:?}): {reply:02x?}");
                false
            }
        },
        None => {
            warn!("No Bluetti answer to {command:?}");
            false
        }
    };
    state.lock().bluetti.last_sent = Some((command, taken, Instant::now()));
    Ok(())
}

/// Feeds queued notifications to the channel and returns what came of them.
fn drain(inbox: &Inbox, channel: &mut Channel) -> Vec<Event> {
    let queued: Vec<Vec<u8>> = inbox
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    let mut events = Vec::new();
    for data in queued {
        match channel.receive(&data, random()) {
            Ok(event) => events.push(event),
            Err(err) => warn!("Bluetti message rejected ({err:?}): {data:02x?}"),
        }
    }
    events
}

/// Waits for the next decrypted reply, or `None` after `REPLY_TIMEOUT`.
async fn wait_reply(
    inbox: &Inbox,
    channel: &mut Channel,
    timer: &mut EspAsyncTimer,
) -> anyhow::Result<Option<Vec<u8>>> {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    while Instant::now() < deadline {
        for event in drain(inbox, channel) {
            if let Event::Data(reply) = event {
                return Ok(Some(reply));
            }
        }
        timer.after(Duration::from_millis(50)).await?;
    }
    Ok(None)
}

fn log_change(before: Option<&Status>, now: &Status) {
    if before.is_none() {
        info!("Bluetti model {:?}", now.model);
    }
    info!(
        "Bluetti: {} % {:.1} V {:.1} A, in solar {} W grid {} W, out AC {} W ({}) DC {} W ({}), \
         to full {} min, to empty {} min, status {}",
        now.soc_pct,
        now.pack_volts,
        now.pack_amps,
        now.pv_in_w,
        now.grid_in_w,
        now.ac_out_w,
        if now.ac_on { "on" } else { "off" },
        now.dc_out_w,
        if now.dc_on { "on" } else { "off" },
        now.minutes_to_full,
        now.minutes_to_empty,
        now.charging_status
    );
}

/// Random bytes from the ESP32's hardware generator.
fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    // SAFETY: writes exactly `N` bytes into a buffer of `N` bytes.
    unsafe { esp_idf_svc::sys::esp_fill_random(out.as_mut_ptr().cast(), N) };
    out
}

async fn write(client: &mut BLEClient, bytes: &[u8]) -> anyhow::Result<()> {
    client
        .get_service(BLUETTI_SERVICE)
        .await?
        .get_characteristic(CHAR_WRITE)
        .await?
        .write_value(bytes, true)
        .await?;
    Ok(())
}
