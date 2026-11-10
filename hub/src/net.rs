//! Wi-Fi for the hub.
//!
//! The hub always runs its own network, so the sensor node and your phone can
//! reach it anywhere. When built with your Wi-Fi details it also joins your
//! network, for internet time and to show the dashboard there too. If that
//! network is out of range, it keeps retrying in the background.
//!
//! Build settings (set as environment variables when building):
//!
//! - `M_PULSE_WIFI_SSID`, `M_PULSE_WIFI_PASS`: your Wi-Fi. Optional.
//! - `M_PULSE_AP_PASS`: password of the hub's own network, at least 8
//!   characters. Defaults to `mpulse-heater`.

use std::thread;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};
use log::{info, warn};

use crate::state::Shared;

/// Name of the hub's own network.
pub const AP_SSID: &str = "M-Pulse";

/// Address of the hub on its own network.
pub const AP_IP: &str = "192.168.4.1";

const AP_PASS: &str = match option_env!("M_PULSE_AP_PASS") {
    Some(pass) => pass,
    None => "mpulse-heater",
};

const HOME_SSID: Option<&str> = option_env!("M_PULSE_WIFI_SSID");
const HOME_PASS: &str = match option_env!("M_PULSE_WIFI_PASS") {
    Some(pass) => pass,
    None => "",
};

/// Pause between attempts to join your network.
const RETRY: Duration = Duration::from_secs(30);

/// Starts Wi-Fi and a background thread that keeps the hub on your network.
pub fn start(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    state: Shared,
) -> anyhow::Result<()> {
    let wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
    let mut wifi = BlockingWifi::wrap(wifi, sysloop)?;

    let ap = AccessPointConfiguration {
        ssid: AP_SSID.try_into().expect("SSID fits"),
        password: AP_PASS
            .try_into()
            .map_err(|_| anyhow::anyhow!("M_PULSE_AP_PASS is too long"))?,
        auth_method: AuthMethod::WPA2Personal,
        channel: 6,
        max_connections: 4,
        ..Default::default()
    };
    anyhow::ensure!(
        AP_PASS.len() >= 8,
        "M_PULSE_AP_PASS needs at least 8 characters"
    );

    let config = match HOME_SSID {
        Some(ssid) => Configuration::Mixed(
            ClientConfiguration {
                ssid: ssid
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("M_PULSE_WIFI_SSID is too long"))?,
                password: HOME_PASS
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("M_PULSE_WIFI_PASS is too long"))?,
                auth_method: if HOME_PASS.is_empty() {
                    AuthMethod::None
                } else {
                    AuthMethod::WPA2Personal
                },
                ..Default::default()
            },
            ap,
        ),
        None => Configuration::AccessPoint(ap),
    };

    wifi.set_configuration(&config)?;
    wifi.start()?;
    info!("Hub network \"{AP_SSID}\" is up, dashboard at http://{AP_IP}");

    thread::Builder::new()
        .name("wifi".into())
        .stack_size(6 * 1024)
        .spawn(move || keep_joined(wifi, state))?;
    Ok(())
}

/// Owns the Wi-Fi driver for the life of the firmware and rejoins your
/// network whenever the link drops.
fn keep_joined(mut wifi: BlockingWifi<EspWifi<'static>>, state: Shared) {
    let Some(ssid) = HOME_SSID else {
        info!("No home Wi-Fi configured, running on the hub network only");
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    };

    loop {
        if !wifi.is_connected().unwrap_or(false) {
            state.lock().sta_ip = None;
            info!("Joining Wi-Fi \"{ssid}\"");
            let joined = wifi.connect().and_then(|()| wifi.wait_netif_up());
            match joined.and_then(|()| wifi.wifi().sta_netif().get_ip_info()) {
                Ok(ip) => {
                    info!("Joined \"{ssid}\", dashboard also at http://{}", ip.ip);
                    state.lock().sta_ip = Some(ip.ip.to_string());
                }
                Err(err) => {
                    warn!(
                        "Could not join \"{ssid}\": {err}. Retrying in {} s",
                        RETRY.as_secs()
                    );
                    let _ = wifi.disconnect();
                }
            }
        }
        thread::sleep(RETRY);
    }
}
