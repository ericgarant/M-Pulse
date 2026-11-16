# Changelog

## Unreleased

- **Adjustable comfort band**: the Climate page has a *Comfort band*
  stepper under the target, ±0.5 to ±3 °C in 0.5 °C steps (±1 °C by
  default), with a line saying where heat starts and stops. It is saved in
  flash, so a restart keeps it. The 10-minute minimum run and rest are
  unchanged, so even the narrowest band starts the heater at most once
  every 20 minutes. `POST /api/band`; limits and the saved record in
  `core/src/thermostat.rs`.
- **Dashboard pages**: the page opens on Home, a summary with a tile per
  service (Climate with the target's − and +, Fridge, Power, Fuel, Rules),
  and each service has its own page behind a tab bar: at the bottom on a
  phone or tablet, down the left side on a laptop. Home lists every alert
  in one line that opens its page; each page shows its own, and tabs get a
  dot. The screen goes back to Home after 5 minutes untouched. About 22 KB
  more page in flash.
- **Rules page**: "Battery rules" is now "Rules", with a third rule, *Heater
  fuel reserve*: the fuel warning, reserve and stop switch moved there from
  Fuel settings.
- **Battery rules kept across a restart**: the low-battery stop's levels and
  the fridge saver's levels and switch are saved in flash (`core/src/rules.rs`).
  The low-battery stop itself always comes back on.
- **Help on every card**: a small (?) button in each card's header opens a
  scrolling sheet that explains each value, control and mode, how the
  automation behaves (Auto vs Monitor only, the comfort band, glow plug
  rest, low-battery stop, fuel estimate, warning and reserve) and what the
  alerts mean. The footer's "Alerts and screen help" covers the top bar,
  alerts and themes. About 13 KB more page in flash.
- **Heater fuel estimate** (no sensor): the hub tallies fuel from burn time
  and power setting (BougeRV's 0.18 to 0.53 L/h), learns the heater's real
  rate from the litres entered at each fill-up, and keeps it all in flash.
  New Heater fuel card on the dashboard with Tank full, a low-fuel warning
  (default 15 %) and a reserve (default 15 %) that stops the heater cleanly
  and blocks starts. `POST /api/fuel`; logic in `core/src/fuel.rs`.

## v1.0.0 (15 Nov 2026)

The first release: everything built up to and including PR #21. Firmware
versions (`hub`, `sensor-node`, `core`) are set to 1.0.0.

### What the system does

- **Hub** (bare ESP32-S3 module with a DHT11 on GPIO4): holds BLE links to
  the BougeRV DHG001 diesel heater, the BougeRV Rocky 50 fridge and the
  Bluetti Elite 200 V2, runs its own `M-Pulse` Wi-Fi and serves the
  dashboard at http://192.168.71.1. No internet, no Home Assistant.
- **Heater thermostat**: deadband around the setpoint, 10-minute minimum on
  and off times for the glow plug, no stop during ignition, up to 3 start
  attempts. Boots in Monitor only; Auto is chosen on the dashboard. Manual
  On and Off, and the burn power in 0.1 kW steps (1.0 to 5.0 kW).
- **Fridge**: target, Eco/Max and power from the dashboard, with a warm alert
  after 30 minutes more than 4 °C above target.
- **Bluetti**: encrypted link, charge, inputs and outputs, and the AC and DC
  output switches (DC off is refused while the heater is not fully off).
- **Battery rules**: the heater low-battery stop (on by default) and the
  fridge saver (Eco while the Bluetti is low).
- **Sensor node**: optional backup cabin sensor over UDP.
- **Dashboard**: one page for the tablet and phone with Climate, Fridge,
  Power and Battery rules cards, alerts, and dark, Night (red) and light
  themes.

### New in this release (PR #21)

Fixes:

- The thermostat could queue the same heater start twice while the first
  was still being written, using up its start attempts early.
- Choosing Auto with the cabin inside the comfort band switched off a heater
  that had been started by hand. Auto now takes over the heater's real state.
- A refused Heater ON or OFF no longer drops Auto back to Monitor.
- A quick second fridge change (Off then Eco) could undo the first. A
  settings write now waits for a status that shows the last change.
- The fridge saver no longer spends its retries while a dashboard command is
  queued.
- A late Bluetti reply could be taken as the answer to an output switch.
- DHT11: reads tenths of a degree and temperatures below 0 °C, and rejects
  an all-zero read that passes the checksum.
- Dashboard: the heater clock detail overwrote the header clock; a slow
  answer could repaint an older state over a newer one; the fridge mode
  flicked back while a change was on its way.
- `/api/time` answers 400, not 500, to a bad body.

Improvements:

- Top-bar chips show each appliance's headline value and turn red on its
  alarm.
- Four-hour trend charts of the cabin (with the comfort band and burning
  time) and of the Bluetti's charge (with the stop level), from the new
  `GET /api/history`.
- Heater ON and OFF are greyed out, with the reason, when the heater cannot
  take them. New alerts for a heater fault code and a Bluetti at 20 % or
  below. 48 px touch targets.
- `/api/state` is written from a copy of the state, so the BLE links never
  wait on it.
- `hub/tools/mock_hub.py` serves the dashboard with made-up readings for
  work without the hardware.

### Known limits

- Settings (setpoint, mode, battery-rule levels) go back to the defaults
  after a restart, and the trend history starts empty.
- The heater and fridge links have not yet been tried on the current hub
  module, and the low-battery stop has not yet stopped a burning heater.
  See [hub/README.md](hub/README.md#tested).
- The heater does not report altitude, efficiency or fuel level over BLE.
