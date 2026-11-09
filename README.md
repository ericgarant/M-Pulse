# M-Pulse

Standalone, decentralized automation for a 2025 Four Wheel Camper (Project M).
Distributed ESP32 nodes written in Rust talk to each other and to appliances
directly over BLE. There is no Home Assistant and no cloud.

## Nodes

| Folder | Board | Purpose | Status |
| --- | --- | --- | --- |
| [`thermostat/`](thermostat/) | Freenove ESP32 WROOM (ESP32-WROOM-32E) | Reads a DHT11 and starts/stops the BougeRV diesel heater over BLE | Phase 1, in progress |

## Getting started

See [docs/mac-setup.md](docs/mac-setup.md) to install the Rust ESP32 toolchain
on macOS, then follow the README in the node folder you want to build.
