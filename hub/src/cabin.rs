//! Receives cabin readings from the sensor node.
//!
//! The sensor node broadcasts one UDP packet per reading (see
//! `m_pulse_core::wire`). Broadcasting means it does not need to know the
//! hub's address, on either network.
//!
//! The hub's own DHT11 (see `dht`) comes first: a packet is ignored while
//! the hub has a fresh reading of its own, so the node is only a fallback.

use std::net::UdpSocket;
use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use m_pulse_core::wire::{self, CabinPacket};

use crate::state::{Cabin, Shared, Source};

/// How long a reading from the hub's own sensor keeps the node's readings
/// out. A few missed 5 s samples are normal; after this the node takes over.
const LOCAL_PRIORITY: Duration = Duration::from_secs(30);

pub fn spawn(state: Shared) -> anyhow::Result<()> {
    thread::Builder::new()
        .name("cabin-rx".into())
        .stack_size(6 * 1024)
        .spawn(move || loop {
            if let Err(err) = receive(&state) {
                warn!("Cabin receiver stopped: {err}. Restarting");
                thread::sleep(Duration::from_secs(5));
            }
        })?;
    Ok(())
}

fn receive(state: &Shared) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(("0.0.0.0", wire::PORT))?;
    info!("Listening for the sensor node on UDP port {}", wire::PORT);

    let mut buf = [0u8; 64];
    loop {
        let (len, from) = socket.recv_from(&mut buf)?;
        let Some(packet) = CabinPacket::parse(&buf[..len]) else {
            warn!("Ignored a malformed packet from {from}");
            continue;
        };

        let previous = {
            let mut s = state.lock();
            let local_fresh = s.cabin.is_some_and(|c| {
                c.source == Source::Local && c.received_at.elapsed() < LOCAL_PRIORITY
            });
            if local_fresh {
                continue;
            }
            s.cabin.replace(Cabin {
                temperature_c: packet.temperature_c,
                humidity_pct: packet.humidity_pct,
                source: Source::Remote,
                received_at: Instant::now(),
            })
        };
        // Log changes only; the node sends the same value every 5 s.
        let changed = previous.is_none_or(|p| {
            p.source != Source::Remote
                || p.temperature_c != packet.temperature_c
                || p.humidity_pct != packet.humidity_pct
        });
        if changed {
            info!(
                "Cabin: {:.1} °C, {:.0} % RH (from {from}, #{})",
                packet.temperature_c, packet.humidity_pct, packet.seq
            );
        }
    }
}
