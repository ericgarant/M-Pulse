# M-Pulse

Standalone, decentralized automation for a 2025 Four Wheel Camper (Project M).
Distributed ESP32 nodes written in Rust talk to each other over their own
Wi-Fi network and to appliances directly over BLE. There is no Home Assistant
and no cloud.

## Nodes

| Folder | Board | Purpose | Status |
| --- | --- | --- | --- |
| [`hub/`](hub/) | Freenove ESP32-S3 media kit | Holds the BLE link to the BougeRV diesel heater, runs the thermostat, switches the heater, serves the dashboard | Phase 1, tested on the heater |
| [`sensor-node/`](sensor-node/) | Freenove ESP32 WROOM (ESP32-WROOM-32E) | Reads a DHT11 and sends cabin readings to the hub over Wi-Fi | Phase 1, tested with the hub |

[`core/`](core/) holds the logic that does not depend on hardware: the
thermostat state machine, the rules for when to switch the heater, sensor
smoothing, local time, the heater's wire format and the sensor node's packet
format. It builds and tests on any computer with `cargo test`.

## Getting started

See [docs/mac-setup.md](docs/mac-setup.md) to install the Rust ESP32 toolchain
on macOS, then follow the README in the node folder you want to build.
