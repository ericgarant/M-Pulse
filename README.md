# M-Pulse

Standalone, decentralized automation for a 2025 Four Wheel Camper (Project M).
Distributed ESP32 nodes written in Rust talk to each other over their own
Wi-Fi network and to appliances directly over BLE. There is no Home Assistant
and no cloud.

## Nodes

| Folder | Board | Purpose | Status |
| --- | --- | --- | --- |
| [`hub/`](hub/) | Bare ESP32-S3 module (16 MB flash, no PSRAM) with a DHT11 on GPIO4 | Reads the cabin temperature, holds the BLE links to the BougeRV diesel heater, Rocky 50 fridge and Bluetti Elite 200 V2, runs the thermostat, switches the heater, controls the fridge, reads the Bluetti, stops the heater and can put the fridge in Eco when the Bluetti runs low, serves the dashboard | Cabin sensor tested on this module. The heater, fridge and Bluetti links were tested on the earlier S3 hub (the Freenove media kit) and still need a check on this one in the camper |
| [`sensor-node/`](sensor-node/) | Freenove ESP32 WROOM (ESP32-WROOM-32E) with a DHT11 | Retired from the camper. The firmware stays as an optional backup: the hub uses its readings only if its own DHT11 stops answering | Tested with the hub before the hub had its own sensor; the handover has not been tested |

[`core/`](core/) holds the logic that does not depend on hardware: the
thermostat state machine, the rules for when to switch the heater, the two
low-battery rules (heater stop and fridge saver), sensor smoothing, local
time, the heater's, fridge's and Bluetti's wire formats (including the
Bluetti's encrypted channel) and the sensor node's packet format. It builds
and tests on any computer with `cargo test`.

## What it does today

- **Heater thermostat.** The hub reads the cabin temperature from its own
  DHT11 (the sensor node is an optional backup), and
  switches the BougeRV diesel heater around a setpoint, with a deadband and
  minimum on and off times to protect the glow plug.
- **Fridge.** The dashboard shows the Rocky 50 and changes its target, mode
  and power. A notice appears if it stays warm.
- **Power.** The dashboard shows the Bluetti's charge, inputs and outputs.
  The hub only reads the Bluetti; it does not switch its outputs.
- **Low battery.** The hub stops the heater cleanly when the Bluetti runs
  low (on by default), and can put the fridge in Eco until it has recharged
  (off until you turn it on).

- **Dashboard.** The hub serves one page for the camper's Android tablet
  and your phone: join the hub's `M-Pulse` Wi-Fi (password `mpulse-heater`
  unless you set your own in `wifi.env`) and open http://192.168.71.1. It
  needs no internet. It has Climate, Fridge, Power and Battery rules cards,
  alerts at the top, and dark, red Night and light themes.

Details, defaults and what has been tested are in [hub/README.md](hub/README.md).

## Hardware

In use:

- **Hub**: a bare ESP32-S3 module (16 MB flash, no PSRAM) with a DHT11 on
  GPIO4 and a 10k pull-up. Wiring is in
  [hub/README.md](hub/README.md#cabin-sensor).
- **Appliances it talks to over BLE**: BougeRV diesel heater (DHG001),
  BougeRV Rocky 50 fridge, Bluetti Elite 200 V2 power station (read only).
- **Screens**: the camper's Android tablet and a phone, both through the
  dashboard in a browser. There is no app to install.

Retired:

- **Freenove ESP32-S3 media kit**: the first hub. Its screen was never used,
  and the firmware no longer depends on anything on that board.
- **Freenove ESP32 WROOM sensor node**: replaced by the hub's own DHT11. Its
  firmware is kept as a backup (see [sensor-node/](sensor-node/)).

Not yet connected: the MaxxAir fans and the ceiling LEDs.

## How it fits together

```
Tablet / phone ── Wi-Fi "M-Pulse" ──┐
  (dashboard at 192.168.71.1)       │
                                    ▼
                        Hub (ESP32-S3 + DHT11) ── BLE ──> BougeRV diesel heater
                                    ▲          ── BLE ──> Rocky 50 fridge
                                    │          ── BLE ──> Bluetti Elite 200 V2
Sensor node (backup) ── UDP 4210 ───┘
```

The hub runs its own Wi-Fi network and needs no router or internet. All
decisions (thermostat, low-battery rules) are made on the hub, so the
dashboard is only a remote control: closing it changes nothing.

## Next steps

Open items, roughly in order:

- Take the new hub to the camper and check that it connects to the heater,
  fridge and Bluetti, all three at once. They were tested on the earlier
  board, and the shorter Bluetooth search time from PR #12 has not yet been
  tried with the devices switched on.
- Try the low-battery stop on a burning heater. So far it has only tripped
  with the heater off.
- Read the Bluetti while it charges from solar and the charger, and under a
  DC load, to check those readings and the sign of the charger input.
- Try the redesigned dashboard on the tablet and phone with live data.
- Remember the setpoint, mode and battery-rule levels across a power cut.
  Today a restart puts the hub in Monitor only with default settings.
- Add buttons to switch the Bluetti's AC and DC outputs. The hub only reads
  the Bluetti today.
- Later: the MaxxAir fans and the ceiling LEDs.

## Getting started

See [docs/mac-setup.md](docs/mac-setup.md) to install the Rust ESP32 toolchain
on macOS, then follow the README in the node folder you want to build.
