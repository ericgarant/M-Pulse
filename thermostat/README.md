# Thermostat node

Firmware for the M-Pulse thermostat: an ESP32 that reads cabin temperature
from a DHT11 and will start and stop the BougeRV diesel heater over BLE.

## Status

Step 2 of 4, heater discovery. The firmware currently:

- reads the DHT11 every 5 seconds on a background thread and logs the result
- scans for the heater, which advertises as `BougeRV-DHG001`
- connects to it and logs every service, characteristic and descriptor
- subscribes to every notification and logs what the heater sends for 90 seconds

It is read-only. It never writes a command to the heater, so it cannot start
or stop it yet.

## Hardware

- Freenove ESP32 WROOM board (ESP32-WROOM-32E module)
- DHT11, bare 4-pin sensor

### Wiring

Hold the DHT11 with the grille facing you and the pins pointing down. Pins are
numbered 1 to 4 from left to right.

| DHT11 pin | Connects to |
| --- | --- |
| 1 (VCC) | `3.3V` on the board |
| 2 (DATA) | `GPIO21` |
| 3 | not connected |
| 4 (GND) | `GND` |

No resistor is needed. The firmware turns on the ESP32's internal pull-up,
which is enough with short wires (roughly 20 cm or less). If the log shows
frequent `DHT11: read attempt ... failed` lines, add a 10k resistor between
DATA and `3.3V`.

The data pin is set in `src/main.rs` (`peripherals.pins.gpio21`).

## Build and flash

Set up the toolchain first: [../docs/mac-setup.md](../docs/mac-setup.md).

```sh
cargo run --release
```

## What you should see

```
I (497) m_pulse_thermostat: M-Pulse thermostat node starting
I (2527) m_pulse_thermostat::sensor: DHT11: 21 °C, 30 % RH
I (4310) m_pulse_thermostat::heater: Heater found: BougeRV-DHG001 at 3C:AB:72:C0:5E:B9 (-65 dBm), ...
I (4900) m_pulse_thermostat::heater: Connected. Reading GATT table
I (5200) m_pulse_thermostat::heater: SERVICE 0x....
I (5400) m_pulse_thermostat::heater:   CHAR 0x....  props=...
I (9000) m_pulse_thermostat::heater: NOTIFY 0x....: [.. .. ..] (n bytes)
```

Close the BougeRV app on your phone first. The heater accepts one BLE
connection at a time and stops advertising while the app is connected.

## Layout

| File | Purpose |
| --- | --- |
| `src/main.rs` | Startup and the main async loop |
| `src/sensor.rs` | DHT11 sampling thread and the shared latest reading |
| `src/ble.rs` | General BLE scan, used when the heater is not found |
| `src/heater.rs` | Heater BLE client: find, connect, list services, log notifications |
| `sdkconfig.defaults` | ESP-IDF settings, including turning NimBLE on |
| `.cargo/config.toml` | Build target, flash runner and ESP-IDF version |
