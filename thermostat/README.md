# Thermostat node

Firmware for the M-Pulse thermostat: an ESP32 that reads cabin temperature
from a DHT11 and will start and stop the BougeRV diesel heater over BLE.

## Status

Step 1 of 4, the scaffold. The firmware currently:

- reads the DHT11 every 5 seconds on a background thread and logs the result
- runs a 10 second BLE scan every 30 seconds and logs every device it hears

It does not talk to the heater yet. The scan output is what we use to find the
heater's name, address and service UUIDs.

## Hardware

- Freenove ESP32 WROOM board (ESP32-WROOM-32E module)
- DHT11, bare 4-pin sensor
- One 10k resistor (4.7k also works)

### Wiring

Hold the DHT11 with the grille facing you and the pins pointing down. Pins are
numbered 1 to 4 from left to right.

| DHT11 pin | Connects to |
| --- | --- |
| 1 (VCC) | `3.3V` on the board |
| 2 (DATA) | `GPIO4`, and through the 10k resistor to `3.3V` |
| 3 | not connected |
| 4 (GND) | `GND` |

The data pin is set in `src/main.rs` (`peripherals.pins.gpio4`).

## Build and flash

Set up the toolchain first: [../docs/mac-setup.md](../docs/mac-setup.md).

```sh
cargo run --release
```

## What you should see

```
I (412) m_pulse_thermostat: M-Pulse thermostat node starting
I (2530) m_pulse_thermostat::sensor: DHT11: 21 °C, 45 % RH
I (10480) m_pulse_thermostat::ble: BLE scan: 3 device(s)
I (10480) m_pulse_thermostat::ble:   AA:BB:CC:DD:EE:FF   -48 dBm  name="..."  services=[...]  mfg=...
I (10490) m_pulse_thermostat: Cabin: 21 °C, 45 % RH (3 s old)
```

To spot the heater, power it on, put the board next to it and close the
BougeRV app on your phone (most BLE devices stop advertising while a phone is
connected). The heater is normally the named device with the strongest signal.

## Layout

| File | Purpose |
| --- | --- |
| `src/main.rs` | Startup and the main async loop |
| `src/sensor.rs` | DHT11 sampling thread and the shared latest reading |
| `src/ble.rs` | BLE scanning. The heater client goes here next |
| `sdkconfig.defaults` | ESP-IDF settings, including turning NimBLE on |
| `.cargo/config.toml` | Build target, flash runner and ESP-IDF version |
