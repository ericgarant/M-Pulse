# Thermostat node

Firmware for the M-Pulse thermostat: an ESP32 that reads cabin temperature
from a DHT11 and will start and stop the BougeRV diesel heater over BLE.

## Status

Steps 2 to 4 of 4 are in progress. The firmware currently:

- reads the DHT11 every 5 seconds on a background thread
- runs the thermostat logic on the smoothed reading and logs each decision.
  This is a dry run: it logs "heat ON" or "heat OFF" and sends nothing
- finds the heater, which advertises as `BougeRV-DHG001`, connects, performs
  the vendor app's opening handshake and logs the heater's status reports

It never sends the heater an on, off or power command yet. Besides the
version request, the only command it sends is the clock sync.

### Thermostat defaults

| Setting | Value |
| --- | --- |
| Setpoint | 18 °C |
| Heat on | at or below 17 °C |
| Heat off | at or above 19 °C |
| Minimum on time | 10 minutes |
| Minimum off time | 10 minutes |
| Sensor timeout | 2 minutes without a reading turns heat off |

They live in `Config::default()` in `../core/src/thermostat.rs`.

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
M_PULSE_CLOCK=$(date +%y%m%d%H%M%S) cargo run --release
```

`M_PULSE_CLOCK` passes your computer's local time into the firmware. The
board has no clock of its own, so it uses this plus the time since boot to
set the heater's clock when it connects, as the BougeRV app does. Flash right
after building so the time is close. Without it, the clock sync is skipped.

## What you should see

```
I (497) m_pulse_thermostat: M-Pulse thermostat node starting
I (510) m_pulse_thermostat::control: Thermostat (dry run): setpoint 18.0 °C, on at or below 17.0 °C, off at or above 19.0 °C
I (2527) m_pulse_thermostat::sensor: DHT11: 21 °C, 30 % RH
I (5520) m_pulse_thermostat::control: Thermostat (dry run): heat OFF, WarmEnough, cabin 21.0 °C
I (4310) m_pulse_thermostat::heater: Heater found: BougeRV-DHG001 at 3C:AB:72:C0:5E:B9 (-65 dBm), ...
I (6200) m_pulse_thermostat::heater: Heater firmware version ...
I (7300) m_pulse_thermostat::heater: STATUS [80 41 00 ff ...] (20 bytes) Status { state: Off, ... }
```

Close the BougeRV app on your phone first. The heater accepts one BLE
connection at a time and stops advertising while the app is connected.

## Layout

| File | Purpose |
| --- | --- |
| `src/main.rs` | Startup and the main async loop |
| `src/sensor.rs` | DHT11 sampling thread and the shared latest reading |
| `src/ble.rs` | General BLE scan, used when the heater is not found |
| `src/heater.rs` | Heater BLE client: find, connect, handshake, log status |
| `src/control.rs` | Thermostat control thread (dry run) |
| `sdkconfig.defaults` | ESP-IDF settings, including turning NimBLE on |
| `.cargo/config.toml` | Build target, flash runner and ESP-IDF version |
