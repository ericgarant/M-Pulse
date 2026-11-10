//! BougeRV diesel heater BLE client.
//!
//! Finds the heater, connects, and performs the same opening handshake as
//! the vendor app: subscribe to replies, ask for the firmware version, then
//! switch to the status stream. After that it only listens and logs.
//!
//! The version request is the only command written to the heater. This code
//! never sends the on/off or power commands.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use embassy_futures::select::{select, Either};
use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{uuid128, BLEAddress, BLEClient, BLEDevice, BLEError, BLEScan};
use esp_idf_svc::timer::EspAsyncTimer;
use log::{info, warn};

use crate::protocol::{self, Status};

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

/// How long to wait for the heater's reply to the version request.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// The vendor app pauses this long between dropping the reply subscription
/// and starting the status subscription.
const SUBSCRIBE_GAP: Duration = Duration::from_millis(400);

/// Give up on a connection attempt after this long. Also guards against the
/// link dropping in the middle of connection setup, which would otherwise
/// leave the connect call waiting forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

// Connection parameters the heater asks for in its 0x2A04 characteristic:
// interval 100 to 200 ms, no latency, 10 s supervision timeout.
const CONN_INTERVAL_MIN: u16 = 80; // units of 1.25 ms
const CONN_INTERVAL_MAX: u16 = 160; // units of 1.25 ms
const CONN_LATENCY: u16 = 0;
const CONN_SUPERVISION_TIMEOUT: u16 = 1000; // units of 10 ms
const CONN_SCAN_INTERVAL: u16 = 16; // units of 0.625 ms
const CONN_SCAN_WINDOW: u16 = 16; // units of 0.625 ms

/// Scans for up to `duration_ms` and returns the heater's address as soon as
/// it is heard, or `None` if it did not advertise in that time.
pub async fn find(device: &BLEDevice, duration_ms: i32) -> Result<Option<BLEAddress>, BLEError> {
    let mut scan = BLEScan::new();
    scan.active_scan(true).interval(100).window(99);

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

/// Connects to the heater, performs the opening handshake, and logs status
/// reports for `listen` before disconnecting.
pub async fn inspect(
    device: &BLEDevice,
    addr: &BLEAddress,
    listen: Duration,
    timer: &mut EspAsyncTimer,
) -> anyhow::Result<()> {
    let mut client = device.new_client();
    client.on_disconnect(|reason| warn!("Heater disconnected (reason {reason:#x})"));
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

    if let Err(err) = handshake(&mut client, timer).await {
        warn!("Handshake stopped early: {err}");
    }

    if client.connected() {
        info!("Listening for {} s", listen.as_secs());
        // Wake once a second so a dropped link ends the wait early.
        let mut remaining = listen.as_secs();
        while remaining > 0 && client.connected() {
            timer.after(Duration::from_secs(1)).await?;
            remaining -= 1;
        }
        if client.connected() {
            info!("Link held for the full {} s", listen.as_secs());
        } else {
            warn!(
                "Link lost {} s into the listen window",
                listen.as_secs() - remaining
            );
        }
    }

    if client.connected() {
        client.disconnect()?;
        info!("Disconnected from heater");
    }

    Ok(())
}

/// The vendor app's opening sequence, minus its clock sync:
///
/// 1. subscribe to replies
/// 2. ask for the firmware version and wait for the answer
/// 3. unsubscribe from replies, pause, subscribe to status
///
/// The app then writes the phone's date and time to the heater. That step is
/// left out on purpose: this board has no real clock, and a wrong time would
/// upset the heater's built-in schedule.
async fn handshake(client: &mut BLEClient, timer: &mut EspAsyncTimer) -> anyhow::Result<()> {
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
                info!(
                    "Heater firmware version {}.{}.{}",
                    data[5], data[6], data[7]
                );
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
    status_chr.on_notify(|data| match Status::parse(data) {
        Some(status) => info!("STATUS {} {status:?}", describe(data)),
        None => warn!("STATUS (unrecognised) {}", describe(data)),
    });
    status_chr.subscribe_notify(true).await?;
    info!("Subscribed to status. Handshake complete");

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
