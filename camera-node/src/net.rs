//! Wi-Fi for the camera node.
//!
//! Tries your Wi-Fi first when it is configured, then the hub's own network,
//! and keeps alternating until one works. Rejoins whenever the link drops.
//! Wi-Fi power save is on, so the radio sleeps between beacons while the
//! camera is in standby. On the hub's network the camera takes start and
//! stop commands only from the hub (see [`Online`]).
//!
//! Build settings (environment variables, the same ones the hub uses):
//!
//! - `M_PULSE_WIFI_SSID`, `M_PULSE_WIFI_PASS`: your Wi-Fi. Optional.
//! - `M_PULSE_AP_PASS`: password of the hub's network. Defaults to
//!   `mpulse-heater`.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use log::{info, warn};

/// Name of the hub's own network. Must match the hub.
const HUB_SSID: &str = "M-Pulse";
const HUB_PASS: &str = match option_env!("M_PULSE_AP_PASS") {
    Some(pass) => pass,
    None => "mpulse-heater",
};

const HOME_SSID: Option<&str> = option_env!("M_PULSE_WIFI_SSID");
const HOME_PASS: &str = match option_env!("M_PULSE_WIFI_PASS") {
    Some(pass) => pass,
    None => "",
};

/// How often the link is checked, and the pause after a failed attempt.
const CHECK_EVERY: Duration = Duration::from_secs(10);

/// Whether the node currently has a network to send on, and whose start and
/// stop commands it takes.
#[derive(Clone)]
pub struct Online {
    up: Arc<AtomicBool>,
    senders: Arc<Mutex<Senders>>,
}

/// Whose start and stop commands count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Senders {
    /// No link, or one being joined: nobody's. Set whenever the link drops, so
    /// an old network's hub address never carries over to the next link.
    Nobody,
    /// On the hub's network: only the hub's, which is the network's gateway.
    Hub(Ipv4Addr),
    /// On your own network, where the hub has another address, or on the
    /// hub's network if it reports no gateway (rather than refusing the hub):
    /// anyone's.
    Anyone,
}

impl Online {
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::Relaxed)
    }

    /// Whether a command from `from` counts: on the hub's network only the
    /// hub's, on your own network anyone's, and with no link nobody's.
    pub fn takes_commands_from(&self, from: IpAddr) -> bool {
        match *self.senders.lock().unwrap_or_else(|e| e.into_inner()) {
            Senders::Nobody => false,
            Senders::Hub(hub) => from == IpAddr::V4(hub),
            Senders::Anyone => true,
        }
    }

    fn senders(&self) -> Senders {
        *self.senders.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_senders(&self, senders: Senders) {
        *self.senders.lock().unwrap_or_else(|e| e.into_inner()) = senders;
    }
}

/// Starts Wi-Fi and a background thread that keeps the node connected.
pub fn start(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
) -> anyhow::Result<Online> {
    let wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
    let mut wifi = BlockingWifi::wrap(wifi, sysloop)?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
    wifi.start()?;
    // Standby: the radio sleeps between beacons. Streaming turns this off.
    set_power_save(true);

    let online = Online {
        up: Arc::new(AtomicBool::new(false)),
        senders: Arc::new(Mutex::new(Senders::Nobody)),
    };
    let flag = online.clone();
    thread::Builder::new()
        .name("wifi".into())
        .stack_size(6 * 1024)
        .spawn(move || keep_joined(wifi, flag))?;
    Ok(online)
}

fn keep_joined(mut wifi: BlockingWifi<EspWifi<'static>>, online: Online) {
    let mut networks: Vec<(&str, &str)> = Vec::new();
    if let Some(ssid) = HOME_SSID {
        networks.push((ssid, HOME_PASS));
    }
    networks.push((HUB_SSID, HUB_PASS));

    let mut next = 0;
    loop {
        let up = wifi.is_up().unwrap_or(false);
        // A join sets whose commands count before the link counts as up, so up
        // with nobody's should not happen. If it does (a failed join whose
        // link came up after all), the camera would announce but refuse the
        // hub: treat the link as down and join again.
        let unheard = up && online.senders() == Senders::Nobody;
        if up && !unheard {
            online.up.store(true, Ordering::Relaxed);
        } else {
            if unheard {
                warn!("Wi-Fi is up but takes nobody's commands: joining again");
            }
            online.up.store(false, Ordering::Relaxed);
            online.set_senders(Senders::Nobody);
            let (ssid, pass) = networks[next];
            next = (next + 1) % networks.len();
            match join(&mut wifi, ssid, pass) {
                Ok((ip, gateway)) => {
                    // On its own network the hub is the gateway.
                    let senders = if ssid != HUB_SSID {
                        info!("Joined \"{ssid}\" as {ip}");
                        Senders::Anyone
                    } else if gateway.is_unspecified() {
                        warn!("Joined \"{ssid}\" as {ip} with no gateway: commands from anyone");
                        Senders::Anyone
                    } else {
                        info!("Joined \"{ssid}\" as {ip}; commands only from the hub at {gateway}");
                        Senders::Hub(gateway)
                    };
                    online.set_senders(senders);
                    online.up.store(true, Ordering::Relaxed);
                }
                Err(err) => {
                    warn!("Could not join \"{ssid}\": {err}");
                    let _ = wifi.disconnect();
                }
            }
        }
        thread::sleep(CHECK_EVERY);
    }
}

fn join(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    ssid: &str,
    pass: &str,
) -> anyhow::Result<(Ipv4Addr, Ipv4Addr)> {
    let _ = wifi.disconnect();
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid
            .try_into()
            .map_err(|_| anyhow::anyhow!("SSID too long"))?,
        password: pass
            .try_into()
            .map_err(|_| anyhow::anyhow!("password too long"))?,
        auth_method: if pass.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        ..Default::default()
    }))?;
    info!("Joining \"{ssid}\"");
    wifi.connect()?;
    wifi.wait_netif_up()?;
    let info = wifi.wifi().sta_netif().get_ip_info()?;
    Ok((info.ip, info.subnet.gateway))
}

/// Wi-Fi power save on in standby (the radio sleeps between beacons) and off
/// while streaming, where it would cut the frame rate.
pub fn set_power_save(on: bool) {
    let mode = if on {
        sys::wifi_ps_type_t_WIFI_PS_MIN_MODEM
    } else {
        sys::wifi_ps_type_t_WIFI_PS_NONE
    };
    let ret = unsafe { sys::esp_wifi_set_ps(mode) };
    if ret != sys::ESP_OK {
        warn!(
            "Could not set Wi-Fi power save {}: {ret}",
            if on { "on" } else { "off" }
        );
    }
}
