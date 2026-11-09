//! BougeRV diesel heater BLE client.
//!
//! For now this is read-only discovery: find the heater, connect, list every
//! service, characteristic and descriptor, and log whatever the heater sends
//! on its own. Nothing is ever written to a heater characteristic here, other
//! than turning notifications on, so the heater cannot be started or stopped
//! by this code. The output tells us where commands and status live.

use std::time::Duration;

use esp32_nimble::{BLEAddress, BLEDevice, BLEError, BLEScan};
use esp_idf_svc::timer::EspAsyncTimer;
use log::{info, warn};

/// The heater advertises as `BougeRV-DHG001`. Matching on the prefix keeps
/// this working if the model suffix differs between units.
const NAME_PREFIX: &str = "BougeRV";

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

/// Connects to the heater, logs its full GATT table, subscribes to every
/// characteristic that can notify or indicate, and then listens for `listen`
/// before disconnecting.
pub async fn inspect(
    device: &BLEDevice,
    addr: &BLEAddress,
    listen: Duration,
    timer: &mut EspAsyncTimer,
) -> anyhow::Result<()> {
    let mut client = device.new_client();
    client.on_disconnect(|reason| warn!("Heater disconnected (reason {reason})"));

    info!("Connecting to heater at {addr}");
    client.connect(addr).await?;
    info!("Connected. Reading GATT table");

    let result = dump_and_subscribe(&mut client).await;
    if let Err(err) = &result {
        warn!("GATT discovery stopped early: {err}");
    }

    if client.connected() {
        info!(
            "Listening for {} s. Use the heater's own controller now to see what changes",
            listen.as_secs()
        );
        timer.after(listen).await?;
    }

    if client.connected() {
        client.disconnect()?;
        info!("Disconnected from heater");
    }

    Ok(())
}

async fn dump_and_subscribe(client: &mut esp32_nimble::BLEClient) -> Result<(), BLEError> {
    for service in client.get_services().await? {
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
                        info!("    DESC {}", desc.uuid());
                    }
                }
                Err(err) => warn!("    descriptor discovery failed: {err}"),
            }

            if chr.can_notify() || chr.can_indicate() {
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
    }

    info!("GATT table complete");
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
