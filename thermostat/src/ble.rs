//! BLE central role.
//!
//! For now this only scans and reports what it hears, which is how we find
//! the heater's advertised name, address and service UUIDs. The heater client
//! (connect, write commands, read status) will be added here next.

use std::collections::BTreeMap;

use esp32_nimble::{BLEDevice, BLEError, BLEScan};
use log::info;

/// What we learned about one advertiser during a scan.
#[derive(Debug, Default)]
pub struct Advertiser {
    pub name: Option<String>,
    pub rssi: i8,
    pub service_uuids: Vec<String>,
    /// Bluetooth SIG company id and the raw manufacturer payload, if present.
    pub manufacturer: Option<(u16, Vec<u8>)>,
}

/// Runs an active scan for `duration_ms` and returns every device heard,
/// keyed by address.
///
/// An active scan also requests each device's scan response, which is where
/// many devices put their name. Advertisement and scan response arrive as
/// separate events, so the fields are merged per address.
pub async fn scan(
    device: &BLEDevice,
    duration_ms: i32,
) -> Result<BTreeMap<String, Advertiser>, BLEError> {
    let mut found: BTreeMap<String, Advertiser> = BTreeMap::new();

    let mut scan = BLEScan::new();
    scan.active_scan(true).interval(100).window(99);

    scan.start(device, duration_ms, |adv, data| {
        let entry = found.entry(adv.addr().to_string()).or_default();
        entry.rssi = adv.rssi();

        if let Some(name) = data.name() {
            entry.name = Some(name.to_string());
        }
        for uuid in data.service_uuids() {
            let uuid = uuid.to_string();
            if !entry.service_uuids.contains(&uuid) {
                entry.service_uuids.push(uuid);
            }
        }
        if let Some(mfg) = data.manufacture_data() {
            entry.manufacturer = Some((mfg.company_identifier, mfg.payload.to_vec()));
        }

        // Returning `None` keeps the scan running for the full duration.
        None::<()>
    })
    .await?;

    Ok(found)
}

/// Logs a scan result, strongest signal first. The heater is usually the
/// strongest named device when the board sits next to it.
pub fn log_scan(found: &BTreeMap<String, Advertiser>) {
    let mut devices: Vec<_> = found.iter().collect();
    devices.sort_by_key(|(_, adv)| std::cmp::Reverse(adv.rssi));

    info!("BLE scan: {} device(s)", devices.len());
    for (addr, adv) in devices {
        info!(
            "  {addr}  {:>4} dBm  name={:?}  services={:?}  mfg={:x?}",
            adv.rssi,
            adv.name.as_deref().unwrap_or("-"),
            adv.service_uuids,
            adv.manufacturer,
        );
    }
}
