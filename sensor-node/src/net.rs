//! Wi-Fi for the sensor node.
//!
//! Tries your Wi-Fi first when it is configured, then the hub's own network,
//! and keeps alternating until one works. Rejoins whenever the link drops.
//!
//! Build settings (environment variables, the same ones the hub uses):
//!
//! - `M_PULSE_WIFI_SSID`, `M_PULSE_WIFI_PASS`: your Wi-Fi. Optional.
//! - `M_PULSE_AP_PASS`: password of the hub's network. Defaults to
//!   `mpulse-heater`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
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

/// Whether the node currently has a network to send on.
#[derive(Clone)]
pub struct Online(Arc<AtomicBool>);

impl Online {
    pub fn is_up(&self) -> bool {
        self.0.load(Ordering::Relaxed)
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

    let online = Online(Arc::new(AtomicBool::new(false)));
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
            online.0.store(true, Ordering::Relaxed);
        } else {
            online.0.store(false, Ordering::Relaxed);
            let (ssid, pass) = networks[next];
            next = (next + 1) % networks.len();
            match join(&mut wifi, ssid, pass) {
                Ok(ip) => {
                    info!("Joined \"{ssid}\" as {ip}");
                    online.0.store(true, Ordering::Relaxed);
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
) -> anyhow::Result<String> {
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
    Ok(wifi.wifi().sta_netif().get_ip_info()?.ip.to_string())
}
