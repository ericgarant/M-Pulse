//! BougeRV diesel heater BLE client.
//!
//! For now this is read-only discovery: find the heater, connect, list every
//! service, characteristic and descriptor, and log whatever the heater sends
//! on its own. Nothing is ever written to a heater characteristic here, other
//! than turning notifications on, so the heater cannot be started or stopped
//! by this code. The output tells us where commands and status live.

use std::time::Duration;

use embassy_futures::select::{select, Either};
use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{
    uuid128, BLEAddress, BLEClient, BLEDevice, BLEError, BLERemoteService, BLEScan,
};
use esp_idf_svc::timer::EspAsyncTimer;
use log::{info, warn};

/// The heater advertises as `BougeRV-DHG001`. Matching on the prefix keeps
/// this working if the model suffix differs between units.
const NAME_PREFIX: &str = "BougeRV";

/// The heater's vendor service. It holds one write characteristic
/// (`bf83f3f1-...`, presumably commands) and two notify characteristics
/// (`bf83f3f2-...` and `bf83f3f3-...`, presumably status).
const HEATER_SERVICE: BleUuid = uuid128!("43f4b114-ca67-48e8-a46f-9a8ffeb7146a");

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

/// Connects to the heater, logs its vendor service first and then the rest
/// of the GATT table, subscribes to the vendor notifications, and listens for
/// `listen` before disconnecting.
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

    // The vendor service comes first, in case the link does not last.
    if let Err(err) = dump_heater_service(&mut client).await {
        warn!("Heater service discovery stopped early: {err}");
    }

    if client.connected() {
        info!(
            "Listening for {} s. Use the heater's own controller now to see what changes",
            listen.as_secs()
        );
        timer.after(listen).await?;
    }

    // The standard services are only informational, so they go last.
    if client.connected() {
        if let Err(err) = dump_other_services(&mut client).await {
            warn!("GATT discovery stopped early: {err}");
        }
    }

    if client.connected() {
        client.disconnect()?;
        info!("Disconnected from heater");
    }

    Ok(())
}

async fn dump_heater_service(client: &mut BLEClient) -> Result<(), BLEError> {
    let service = client.get_service(HEATER_SERVICE).await?;
    dump_service(service, true).await?;
    info!("Heater service complete");
    Ok(())
}

async fn dump_other_services(client: &mut BLEClient) -> Result<(), BLEError> {
    for service in client.get_services().await? {
        if service.uuid() != HEATER_SERVICE {
            dump_service(service, false).await?;
        }
    }
    info!("GATT table complete");
    Ok(())
}

/// Logs one service. With `subscribe`, also turns on notifications for every
/// characteristic that offers them and logs what arrives.
async fn dump_service(service: &mut BLERemoteService, subscribe: bool) -> Result<(), BLEError> {
    info!("SERVICE {}", service.uuid());

    for chr in service.get_characteristics().await? {
        let uuid = chr.uuid();
        info!("  CHAR {uuid}  props={:?}", chr.properties());

        if chr.can_read() {
            match chr.read_value().await {
                Ok(value) => info!("    value: {}", describe(&value)),
                Err(err) => warn!("    read failed: {err}"),
            }
        }

        match chr.get_descriptors().await {
            Ok(descriptors) => {
                for desc in descriptors {
                    // 0x2902 is the notification switch. Everything else,
                    // such as the 0x2901 text label, is worth reading.
                    if desc.uuid() == BleUuid::from_uuid16(0x2902) {
                        info!("    DESC {}", desc.uuid());
                        continue;
                    }
                    match desc.read_value().await {
                        Ok(value) => info!("    DESC {}: {}", desc.uuid(), describe(&value)),
                        Err(err) => warn!("    DESC {}: read failed: {err}", desc.uuid()),
                    }
                }
            }
            Err(err) => warn!("    descriptor discovery failed: {err}"),
        }

        if subscribe && (chr.can_notify() || chr.can_indicate()) {
            chr.on_notify(move |data| info!("NOTIFY {uuid}: {}", describe(data)));

            let subscribed = if chr.can_notify() {
                chr.subscribe_notify(true).await
            } else {
                chr.subscribe_indicate(true).await
            };
            match subscribed {
                Ok(()) => info!("    subscribed"),
                Err(err) => warn!("    subscribe failed: {err}"),
            }
        }
    }

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
