# Hub (ESP32-S3)

Firmware for the Freenove ESP32-S3 media kit board. It:

- keeps a BLE link to the BougeRV diesel heater (`BougeRV-DHG001`) and
  reconnects if it drops
- receives cabin readings from the [sensor node](../sensor-node) over Wi-Fi
- runs the thermostat on those readings. **Dry run**: it decides but never
  switches the heater. The only commands it writes are the version request
  and the clock sync
- serves a dashboard at http://192.168.4.1 (hub network) and at its address
  on your Wi-Fi
- sets the heater's clock from internet time (SNTP), or from your phone's
  time when you open the dashboard without internet

The heater link lives here because the classic ESP32 could not hold it: the
heater went silent about 11 s into every connection. The S3 and a Mac both
hold the same link indefinitely.

## Networks

The hub always runs its own Wi-Fi network, `M-Pulse`. If you build it with
your Wi-Fi details it also joins your network, and keeps retrying when it is
out of range.

Put the settings in `wifi.env` at the top of the repository. Git ignores this
file, so the passwords stay on your computer:

```sh
export M_PULSE_WIFI_SSID="your network"
export M_PULSE_WIFI_PASS="your password"
export M_PULSE_AP_PASS="password for the M-Pulse network"   # 8+ characters
```

Leave out the first two lines to use only the hub network. Without
`M_PULSE_AP_PASS` the hub network password is `mpulse-heater`.

## Build and flash

Set up the toolchain first: [../docs/mac-setup.md](../docs/mac-setup.md).

```sh
source ../wifi.env
cargo build --release
espflash flash --monitor --partition-table partitions.csv target/xtensa-esp32s3-espidf/release/m-pulse-hub
```

## Thermostat defaults

| Setting | Value |
| --- | --- |
| Setpoint | 18 °C (change it on the dashboard) |
| Heat on | at or below setpoint − 1 °C |
| Heat off | at or above setpoint + 1 °C |
| Minimum on time | 10 minutes |
| Minimum off time | 10 minutes |
| Sensor timeout | 2 minutes without a reading turns heat off |

They live in `Config::default()` in `../core/src/thermostat.rs`. A setpoint
change from the dashboard lasts until the hub restarts.
