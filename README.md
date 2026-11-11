# M-Pulse

Standalone, decentralized automation for a 2025 Four Wheel Camper (Project M).
Distributed ESP32 nodes written in Rust talk to each other over their own
Wi-Fi network and to appliances directly over BLE. There is no Home Assistant
and no cloud.

## Nodes

| Folder | Board | Purpose | Status |
| --- | --- | --- | --- |
| [`hub/`](hub/) | Freenove ESP32-S3 media kit | Holds the BLE links to the BougeRV diesel heater, Rocky 50 fridge and Bluetti Elite 200 V2, runs the thermostat, switches the heater, controls the fridge, reads the Bluetti, stops the heater and can put the fridge in Eco when the Bluetti runs low, serves the dashboard | Tested on all three; the low-battery heater stop has not yet been tried on a burning heater |
| [`sensor-node/`](sensor-node/) | Freenove ESP32 WROOM (ESP32-WROOM-32E) | Reads a DHT11 and sends cabin readings to the hub over Wi-Fi | Phase 1, tested with the hub |

[`core/`](core/) holds the logic that does not depend on hardware: the
thermostat state machine, the rules for when to switch the heater, the two
low-battery rules (heater stop and fridge saver), sensor smoothing, local
time, the heater's, fridge's and Bluetti's wire formats (including the
Bluetti's encrypted channel) and the sensor node's packet format. It builds
and tests on any computer with `cargo test`.

## What it does today

- **Heater thermostat.** The sensor node reads the cabin temperature; the hub
  switches the BougeRV diesel heater around a setpoint, with a deadband and
  minimum on and off times to protect the glow plug.
- **Fridge.** The dashboard shows the Rocky 50 and changes its target, mode
  and power. A notice appears if it stays warm.
- **Power.** The dashboard shows the Bluetti's charge, inputs and outputs.
  The hub only reads the Bluetti; it does not switch its outputs.
- **Low battery.** The hub stops the heater cleanly when the Bluetti runs
  low (on by default), and can put the fridge in Eco until it has recharged
  (off until you turn it on).

Details, defaults and what has been tested are in [hub/README.md](hub/README.md).

## Getting started

See [docs/mac-setup.md](docs/mac-setup.md) to install the Rust ESP32 toolchain
on macOS, then follow the README in the node folder you want to build.
