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
    /// On the hub's network, the hub's address (the network's gateway): the
    /// only sender whose commands count. `None` on your own network, where
    /// the hub has another address, so commands from any sender count.
    hub: Arc<Mutex<Option<Ipv4Addr>>>,
}

impl Online {
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::Relaxed)
    }

    /// Whether a command from `from` counts: on the hub's network only the
    /// hub's, on your own network anyone's.
    pub fn takes_commands_from(&self, from: IpAddr) -> bool {
        match *self.hub.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(hub) => from == IpAddr::V4(hub),
            None => true,
        }
    }

    fn set_hub(&self, hub: Option<Ipv4Addr>) {
        *self.hub.lock().unwrap_or_else(|e| e.into_inner()) = hub;
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
        hub: Arc::new(Mutex::new(None)),
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
        if wifi.is_up().unwrap_or(false) {
            online.up.store(true, Ordering::Relaxed);
        } else {
            online.up.store(false, Ordering::Relaxed);
            let (ssid, pass) = networks[next];
            next = (next + 1) % networks.len();
            match join(&mut wifi, ssid, pass) {
                Ok((ip, gateway)) => {
                    // On its own network the hub is the gateway. A network with
                    // no gateway leaves commands open rather than refusing the hub.
                    let hub = (ssid == HUB_SSID && !gateway.is_unspecified()).then_some(gateway);
                    match hub {
                        Some(hub) => info!("Joined \"{ssid}\" as {ip}; commands only from the hub at {hub}"),
                        None => info!("Joined \"{ssid}\" as {ip}"),
                    }
                    online.set_hub(hub);
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
        warn!("Could not set Wi-Fi power save {}: {ret}", if on { "on" } else { "off" });
    }
}
