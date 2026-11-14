//! BougeRV diesel heater BLE client.
//!
//! Keeps a lasting link to the heater: finds it, connects, performs the same
//! opening handshake as the vendor app (subscribe to replies, ask for the
//! firmware version, switch to the status stream), sets the heater's clock
//! once the hub knows the time, and publishes every status report. If the
//! link drops, it reconnects.
//!
//! Besides the version request and the clock sync, two commands are written
//! to the heater, and only when another part of the hub queues one in the
//! shared state: the on/off switch (the dashboard's On and Off buttons, or
//! the thermostat in Auto mode) and the power level (the dashboard's + and -
//! buttons only; the thermostat never changes the power).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use embassy_futures::select::{select, Either};
use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{uuid128, BLEAddress, BLEClient, BLEDevice, BLEError, BLEScan};
use esp_idf_svc::timer::EspAsyncTimer;
use log::{error, info, warn};
use m_pulse_core::protocol::{self, Status};

use crate::state::Shared;
use crate::{clock, radio};

/// The heater advertises as `BougeRV-DHG001`. Matching on the prefix keeps
/// this working if the model suffix differs between units.
const NAME_PREFIX: &str = "BougeRV";

/// The heater's vendor service and its three characteristics. The names in
/// quotes are the labels the heater gives them in its 0x2901 descriptors.
const HEATER_SERVICE: BleUuid = uuid128!("43f4b114-ca67-48e8-a46f-9a8ffeb7146a");
/// "RX cmd.": commands are written here.
const CHAR_COMMAND: BleUuid = uuid128!("bf83f3f1-399a-414d-9035-ce64ceb3ff67");
/// "TX ask.": replies to commands arrive here as notifications.
const CHAR_REPLY: BleUuid = uuid128!("bf83f3f2-399a-414d-9035-ce64ceb3ff67");
/// "TX dat.": a status report arrives here every second.
const CHAR_STATUS: BleUuid = uuid128!("bf83f3f3-399a-414d-9035-ce64ceb3ff67");

/// Largest radio packet we send, in bytes, and its airtime in µs. NimBLE
/// asks for 251 bytes on every new link. This caps it at the Bluetooth 4.0
/// minimum of 27, which is what the ESP32-S3 link test that held used.
const DATA_LEN_TX_OCTETS: u16 = 27;
const DATA_LEN_TX_TIME_US: u16 = 328;

/// Overrides NimBLE's automatic 251-byte data length request.
fn limit_packet_size(client: &BLEClient) {
    let handle = match client.desc() {
        Ok(desc) => desc.conn_handle(),
        Err(err) => {
            warn!("No connection handle for the data length request: {err:?}");
            return;
        }
    };
    // SAFETY: plain NimBLE host call on a live connection handle.
    let rc = unsafe {
        esp_idf_svc::sys::ble_gap_set_data_len(handle, DATA_LEN_TX_OCTETS, DATA_LEN_TX_TIME_US)
    };
    if rc == 0 {
        info!("Asked for {DATA_LEN_TX_OCTETS}-byte packets");
    } else {
        warn!("Data length request failed (rc {rc})");
    }
}

/// How long to wait for the heater's reply to the version request.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// The vendor app pauses this long between dropping the reply subscription
/// and starting the status subscription.
const SUBSCRIBE_GAP: Duration = Duration::from_millis(400);

/// Give up on a connection attempt after this long. Also guards against the
/// link dropping in the middle of connection setup, which would otherwise
/// leave the connect call waiting forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

// Connection parameters the vendor app settles on with this heater, read
// from the phone's Bluetooth log: 90 ms interval, no latency, 3 s
// supervision timeout. (The heater's own 0x2A04 asks for 100 to 200 ms and
// 10 s, but the link dropped with those.)
const CONN_INTERVAL_MIN: u16 = 72; // units of 1.25 ms
const CONN_INTERVAL_MAX: u16 = 72; // units of 1.25 ms
const CONN_LATENCY: u16 = 0;
const CONN_SUPERVISION_TIMEOUT: u16 = 300; // units of 10 ms
const CONN_SCAN_INTERVAL: u16 = 16; // units of 0.625 ms
const CONN_SCAN_WINDOW: u16 = 16; // units of 0.625 ms

/// How long to look for the heater in each attempt.
const FIND_DURATION_MS: i32 = 15_000;

/// Pause before looking for the heater again after a link ends.
const RETRY_PAUSE: Duration = Duration::from_secs(10);

/// The heater reports once a second. If it goes quiet this long while
/// connected, the link is closed and rebuilt.
const STATUS_SILENCE_LIMIT: Duration = Duration::from_secs(15);

/// Keeps the heater link up for the life of the firmware.
pub async fn run(
    device: &BLEDevice,
    timer: &mut EspAsyncTimer,
    state: Shared,
) -> anyhow::Result<()> {
    loop {
        // Search and connect only while the fridge link is not doing the same.
        let turn = radio::take(timer).await?;
        match find(device, FIND_DURATION_MS).await {
            Ok(Some(addr)) => {
                if let Err(err) = session(device, &addr, timer, &state, turn).await {
                    warn!("Heater link ended: {err}");
                }
            }
            // Hand the radio on before the pause below, or this link takes
            // it straight back and the others never get to search.
            Ok(None) => {
                drop(turn);
                info!("Heater not in range");
            }
            Err(err) => {
                drop(turn);
                error!("Heater search failed: {err}");
            }
        }
        {
            let mut s = state.lock();
            s.heater.connected = false;
            s.heater.clock_synced = false;
            // A command must never fire late, after a reconnect.
            s.heater.pending = None;
            s.heater.pending_power = None;
        }
        timer.after(RETRY_PAUSE).await?;
    }
}

/// Scans for up to `duration_ms` and returns the heater's address as soon as
/// it is heard, or `None` if it did not advertise in that time.
pub async fn find(device: &BLEDevice, duration_ms: i32) -> Result<Option<BLEAddress>, BLEError> {
    let mut scan = BLEScan::new();
    scan.active_scan(true)
        .interval(radio::SCAN_INTERVAL)
        .window(radio::SCAN_WINDOW);

    scan.start(device, duration_ms, |adv, data| {
        let name = data.name()?;
        if !name.to_string().starts_with(NAME_PREFIX) {
            return None;
        }
        info!(
            "Heater found: {} at {} ({} dBm), mfg={:x?}",
            name,
            adv.addr(),
            adv.rssi(),
            data.manufacture_data().map(|m| m.payload.to_vec()),
        );
        // Returning `Some` stops the scan early.
        Some(adv.addr())
    })
    .await
}

/// One connection: connect, handshake, then stay connected until the link
/// drops or the heater goes quiet.
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
        warn!("Heater disconnected (reason {reason:#x})");
        on_drop.lock().heater.connected = false;
    });
    client.set_connection_params(
        CONN_INTERVAL_MIN,
        CONN_INTERVAL_MAX,
        CONN_LATENCY,
        CONN_SUPERVISION_TIMEOUT,
        CONN_SCAN_INTERVAL,
        CONN_SCAN_WINDOW,
    );

    info!("Connecting to heater at {addr}");
    match select(client.connect(addr), timer.after(CONNECT_TIMEOUT)).await {
        Either::First(connected) => connected?,
        Either::Second(_) => {
            // Make sure nothing is left half-open before the next attempt.
            if client.connected() {
                let _ = client.disconnect();
            }
            anyhow::bail!("connection attempt timed out");
        }
    }
    info!("Connected, RSSI {:?} dBm", client.get_rssi().ok());
    limit_packet_size(&client);
    drop(turn);
    {
        let mut s = state.lock();
        s.heater.connected = true;
        s.heater.clock_synced = false;
        s.heater.status_at = None;
    }

    let started = Instant::now();
    let result = stay_connected(&mut client, timer, state).await;

    if client.connected() {
        // A clean disconnect leaves the heater ready for the next connection.
        let _ = client.disconnect();
    }
    info!("Link lasted {} s", started.elapsed().as_secs());
    result
}

async fn stay_connected(
    client: &mut BLEClient,
    timer: &mut EspAsyncTimer,
    state: &Shared,
) -> anyhow::Result<()> {
    handshake(client, timer, state).await?;
    let subscribed_at = Instant::now();

    while client.connected() {
        if !state.lock().heater.clock_synced {
            if let Some(now) = clock::local_now() {
                let sync = protocol::clock_frame(&now);
                info!("Writing clock sync {now:?} as {}", describe(&sync));
                write_command(client, &sync).await?;
                state.lock().heater.clock_synced = true;
            }
        }

        let queued = state.lock().heater.pending.take();
        if let Some(switch) = queued {
            let frame = protocol::switch_frame(switch);
            warn!("Switching heater {switch:?}: writing {}", describe(&frame));
            write_command(client, &frame).await?;
            state.lock().heater.last_sent = Some((switch, Instant::now()));
        }

        let queued = state.lock().heater.pending_power.take();
        if let Some(kw_x10) = queued {
            // The web handler already checked the range; this is the last
            // guard before anything reaches the heater.
            match protocol::power_frame(kw_x10) {
                Some(frame) => {
                    warn!(
                        "Setting heater power to {}.{} kW: writing {}",
                        kw_x10 / 10,
                        kw_x10 % 10,
                        describe(&frame)
                    );
                    write_command(client, &frame).await?;
                    state.lock().heater.last_power_sent = Some((kw_x10, Instant::now()));
                }
                None => warn!("Dropped out-of-range power level {kw_x10}"),
            }
        }

        let last_status = state.lock().heater.status_at.unwrap_or(subscribed_at);
        if last_status.elapsed() > STATUS_SILENCE_LIMIT {
            anyhow::bail!("no status for {} s", STATUS_SILENCE_LIMIT.as_secs());
        }

        timer.after(Duration::from_secs(1)).await?;
    }
    anyhow::bail!("link lost")
}

async fn write_command(client: &mut BLEClient, frame: &[u8]) -> anyhow::Result<()> {
    client
        .get_service(HEATER_SERVICE)
        .await?
        .get_characteristic(CHAR_COMMAND)
        .await?
        .write_value(frame, true)
        .await?;
    Ok(())
}

/// The vendor app's opening sequence:
///
/// 1. subscribe to replies
/// 2. ask for the firmware version and wait for the answer
/// 3. unsubscribe from replies, pause, subscribe to status
///
/// The app then writes the current time; `stay_connected` does that as soon
/// as the hub's clock is set.
async fn handshake(
    client: &mut BLEClient,
    timer: &mut EspAsyncTimer,
    state: &Shared,
) -> anyhow::Result<()> {
    let service = client.get_service(HEATER_SERVICE).await?;

    // 1. Replies.
    let reply: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let reply_slot = reply.clone();
    let reply_chr = service.get_characteristic(CHAR_REPLY).await?;
    reply_chr.on_notify(move |data| {
        info!("REPLY {}", describe(data));
        *reply_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(data.to_vec());
    });
    reply_chr.subscribe_notify(true).await?;
    info!("Subscribed to replies");

    // 2. Version request.
    let request = protocol::frame(protocol::CMD_VERSION, &[]);
    info!("Writing version request {}", describe(&request));
    service
        .get_characteristic(CHAR_COMMAND)
        .await?
        .write_value(&request, true)
        .await?;

    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(100);
    loop {
        if let Some(data) = reply.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if data.len() >= 8 && data[1] == protocol::CMD_VERSION {
                let version = format!("{}.{}.{}", data[5], data[6], data[7]);
                info!("Heater firmware version {version}");
                state.lock().heater.firmware = Some(version);
            }
            break;
        }
        if waited >= REPLY_TIMEOUT {
            warn!("No reply to the version request, carrying on");
            break;
        }
        timer.after(step).await?;
        waited += step;
    }

    // 3. Swap the reply subscription for the status stream.
    let reply_chr = service.get_characteristic(CHAR_REPLY).await?;
    if let Err(err) = reply_chr.unsubscribe(true).await {
        warn!("Unsubscribing from replies failed: {err}");
    }
    timer.after(SUBSCRIBE_GAP).await?;

    let status_chr = service.get_characteristic(CHAR_STATUS).await?;
    let publish = state.clone();
    let mut last: Option<Status> = None;
    let mut last_raw: Option<[u8; 20]> = None;
    status_chr.on_notify(move |data| match Status::parse(data) {
        Some(status) => {
            let raw: Option<[u8; 20]> = data.try_into().ok();
            // Log changes only; the heater repeats itself every second. The
            // raw bytes are logged too, so undecoded fields can be matched
            // against the LCD remote from a log.
            if last != Some(status) || last_raw != raw {
                info!("Heater: {status:?} raw {}", describe(data));
                last = Some(status);
                last_raw = raw;
            }
            let mut s = publish.lock();
            s.heater.status = Some(status);
            s.heater.status_at = Some(Instant::now());
            s.heater.raw_status = raw;
        }
        None => warn!("STATUS (unrecognised) {}", describe(data)),
    });
    status_chr.subscribe_notify(true).await?;
    info!("Subscribed to status");
    info!("Handshake complete");

    Ok(())
}

/// Formats bytes as hex, followed by the text form when it is printable.
fn describe(bytes: &[u8]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut out = format!("[{}] ({} bytes)", hex.join(" "), bytes.len());

    let printable = !bytes.is_empty() && bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ');
    if printable {
        out.push_str(&format!(" \"{}\"", String::from_utf8_lossy(bytes)));
    }
    out
}
