# Sensor node (ESP32)

Firmware for the Freenove ESP32 WROOM board with the DHT11. It reads the
cabin temperature and humidity every 5 seconds and broadcasts each reading to
the [hub](../hub) over the hub's `M-Pulse` Wi-Fi network (UDP port 4210).
Readings older than 30 seconds are not sent, so a broken sensor looks lost to
the hub. The hub's thermostat is the only thing that uses these readings; the
fridge and Bluetti features do not need this board. If the optional home Wi-Fi is configured (see the hub README), it
tries that network first and then the hub's, until one works.

## Wiring

Hold the DHT11 with the grille facing you and the pins pointing down. Pins are
numbered 1 to 4 from left to right.

| DHT11 pin | Connects to |
| --- | --- |
| 1 (VCC) | `3.3V` on the board |
| 2 (DATA) | `GPIO21`, plus a 10k resistor to `3.3V` |
| 3 | not connected |
| 4 (GND) | `GND` |

The data pin is set in `src/main.rs` (`peripherals.pins.gpio21`).

## Build and flash

Uses the same `wifi.env` as the hub (see [../hub/README.md](../hub/README.md)),
so both boards agree on the network password. If you have no `wifi.env`,
skip the `source` line.

```sh
source ../wifi.env
cargo build --release
espflash flash --monitor --partition-table partitions.csv target/xtensa-esp32-espidf/release/m-pulse-sensor-node
```

To watch the readings arrive from a computer that has joined `M-Pulse`:
`nc -ulk 4210`.
