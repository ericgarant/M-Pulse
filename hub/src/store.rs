//! Keeps settings in flash, so a power cut does not lose them.
//!
//! Each record is kept as two copies, under two keys, and each save
//! overwrites the older one. ESP-IDF erases a key before writing it, so with
//! a single key a power cut at the wrong moment would lose the record; with
//! two, the other copy is still there. Each copy carries a sequence number
//! and a checksum, and loading picks the newest good one.
//!
//! Four records use it: the fuel gauge (`m_pulse_core::fuel::Saved`), the
//! battery rules (`m_pulse_core::rules::Saved`), the thermostat's
//! comfort band (`m_pulse_core::thermostat::Saved`) and the camera
//! rotations (`crate::camera::Saved`).

use std::fmt::Debug;

use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
use log::{info, warn};
use m_pulse_core::{fuel, rules, thermostat};

use crate::camera;

/// Something saved by a [`Store`].
pub trait Record: Sized + Debug {
    /// The NVS namespace, and the log name.
    const NAMESPACE: &'static str;
    const NAME: &'static str;
    const KEYS: [&'static str; 2];
    const LEN: usize;
    fn encode(&self, seq: u32) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Option<(Self, u32)>;
}

impl Record for fuel::Saved {
    const NAMESPACE: &'static str = "fuel";
    const NAME: &'static str = "Fuel gauge";
    const KEYS: [&'static str; 2] = ["gauge_a", "gauge_b"];
    const LEN: usize = fuel::Saved::LEN;
    fn encode(&self, seq: u32) -> Vec<u8> {
        fuel::Saved::encode(self, seq).to_vec()
    }
    fn decode(bytes: &[u8]) -> Option<(Self, u32)> {
        fuel::Saved::decode(bytes)
    }
}

impl Record for rules::Saved {
    const NAMESPACE: &'static str = "rules";
    const NAME: &'static str = "Battery rules";
    const KEYS: [&'static str; 2] = ["rules_a", "rules_b"];
    const LEN: usize = rules::Saved::LEN;
    fn encode(&self, seq: u32) -> Vec<u8> {
        rules::Saved::encode(self, seq).to_vec()
    }
    fn decode(bytes: &[u8]) -> Option<(Self, u32)> {
        rules::Saved::decode(bytes)
    }
}

impl Record for thermostat::Saved {
    const NAMESPACE: &'static str = "thermo";
    const NAME: &'static str = "Thermostat";
    const KEYS: [&'static str; 2] = ["thermo_a", "thermo_b"];
    const LEN: usize = thermostat::Saved::LEN;
    fn encode(&self, seq: u32) -> Vec<u8> {
        thermostat::Saved::encode(self, seq).to_vec()
    }
    fn decode(bytes: &[u8]) -> Option<(Self, u32)> {
        thermostat::Saved::decode(bytes)
    }
}

impl Record for camera::Saved {
    const NAMESPACE: &'static str = "cameras";
    const NAME: &'static str = "Camera rotations";
    const KEYS: [&'static str; 2] = ["rot_a", "rot_b"];
    const LEN: usize = camera::Saved::LEN;
    fn encode(&self, seq: u32) -> Vec<u8> {
        camera::Saved::encode(self, seq)
    }
    fn decode(bytes: &[u8]) -> Option<(Self, u32)> {
        camera::Saved::decode(bytes)
    }
}

pub struct Store<R: Record> {
    nvs: EspDefaultNvs,
    /// The sequence number of the newest copy in flash.
    seq: u32,
    _record: std::marker::PhantomData<R>,
}

impl<R: Record> Store<R> {
    /// Opens the store and loads the newest good copy, if any.
    pub fn open(partition: EspDefaultNvsPartition) -> anyhow::Result<(Self, Option<R>)> {
        let nvs = EspDefaultNvs::new(partition, R::NAMESPACE, true)?;
        let mut newest: Option<(R, u32)> = None;
        for key in R::KEYS {
            let mut buf = vec![0u8; R::LEN];
            let copy = match nvs.get_blob(key, &mut buf) {
                Ok(Some(bytes)) => R::decode(bytes),
                Ok(None) => None,
                Err(err) => {
                    warn!("{}: could not read {key}: {err}", R::NAME);
                    None
                }
            };
            if let Some((saved, seq)) = copy {
                if newest.as_ref().is_none_or(|(_, best)| seq > *best) {
                    newest = Some((saved, seq));
                }
            }
        }
        match &newest {
            Some((saved, _)) => info!("{}: loaded {saved:?}", R::NAME),
            None => info!("{}: nothing saved yet", R::NAME),
        }
        let seq = newest.as_ref().map_or(0, |(_, seq)| *seq);
        let store = Self {
            nvs,
            seq,
            _record: std::marker::PhantomData,
        };
        Ok((store, newest.map(|(saved, _)| saved)))
    }

    /// Writes a new copy over the older of the two.
    pub fn save(&mut self, saved: &R) -> anyhow::Result<()> {
        let seq = self.seq.wrapping_add(1);
        let key = R::KEYS[(seq % 2) as usize];
        self.nvs.set_blob(key, &saved.encode(seq))?;
        self.seq = seq;
        Ok(())
    }
}
