//! Keeps the fuel gauge in flash, so a power cut does not lose it.
//!
//! Two copies are kept, under two keys, and each save overwrites the older
//! one. ESP-IDF erases a key before writing it, so with a single key a power
//! cut at the wrong moment would lose the gauge; with two, the other copy is
//! still there. Each copy carries a sequence number and a checksum (see
//! `m_pulse_core::fuel::Saved`), and loading picks the newest good one.

use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
use log::{info, warn};
use m_pulse_core::fuel::Saved;

const NAMESPACE: &str = "fuel";
const KEYS: [&str; 2] = ["gauge_a", "gauge_b"];

pub struct FuelStore {
    nvs: EspDefaultNvs,
    /// The sequence number of the newest copy in flash.
    seq: u32,
}

impl FuelStore {
    /// Opens the store and loads the newest good copy, if any.
    pub fn open(partition: EspDefaultNvsPartition) -> anyhow::Result<(Self, Option<Saved>)> {
        let nvs = EspDefaultNvs::new(partition, NAMESPACE, true)?;
        let mut newest: Option<(Saved, u32)> = None;
        for key in KEYS {
            let mut buf = [0u8; Saved::LEN];
            let copy = match nvs.get_blob(key, &mut buf) {
                Ok(Some(bytes)) => Saved::decode(bytes),
                Ok(None) => None,
                Err(err) => {
                    warn!("Fuel gauge: could not read {key}: {err}");
                    None
                }
            };
            if let Some((saved, seq)) = copy {
                if newest.is_none_or(|(_, best)| seq > best) {
                    newest = Some((saved, seq));
                }
            }
        }
        match newest {
            Some((saved, _)) => info!("Fuel gauge: loaded {saved:?}"),
            None => info!("Fuel gauge: nothing saved yet"),
        }
        let seq = newest.map_or(0, |(_, seq)| seq);
        Ok((Self { nvs, seq }, newest.map(|(saved, _)| saved)))
    }

    /// Writes a new copy over the older of the two.
    pub fn save(&mut self, saved: &Saved) -> anyhow::Result<()> {
        let seq = self.seq.wrapping_add(1);
        let key = KEYS[(seq % 2) as usize];
        self.nvs.set_blob(key, &saved.encode(seq))?;
        self.seq = seq;
        Ok(())
    }
}
