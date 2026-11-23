# Changelog

## Unreleased

- **Cameras (hub)**: keeps a list of up to 8 cameras announced on UDP port
  4220 and starts them only while a viewer asks. New Cameras page with Watch,
  Rotate and Stop, a Home tile, and `GET /api/cameras` with `POST
  /api/cameras/<name>/start` or `/stop`. The access point now takes 8
  connections, up from 4. See `hub/README.md` (Cameras).
- **Cameras (camera node)**: new firmware in `camera-node/` for the Freenove
  ESP32-WROVER with an OV3660. It waits in standby until the hub asks, then
  streams MJPEG, and returns to standby on stop or, as a backup, after 5
  minutes with no viewer.
  Name set with `M_PULSE_CAMERA_NAME`. An OV2640 board has also been tested.
- **Cameras (core)**: `core/src/camera.rs` holds the UDP messages (announce,
  start, stop), the camera states and the name rules, with tests.
- **Camera rotation (hub)**: each camera's picture rotation (0, 90, 180 or
  270 degrees) is saved on the hub in flash and kept across reboots, set by
  `POST /api/cameras/<name>/rotation` with the number as the body.
- **Camera video keeps playing (hub)**: while the Cameras page is open and a
  tile is on screen, the page keeps renewing the camera, even while the
  video is paused, until you press Stop or leave the page.
- **Camera photos (hub)**: a Photo button saves a still JPEG to the device.
- **Camera quality (camera node)**: default SVGA (800 by 600) at quality 12.
  `GET` and `POST /settings` read and change the frame size and quality, and
  a Quality (test) picker on each tile uses it. The stream moved to its own
  server on port 81, so a photo or a settings change no longer waits for the
  stream. Wi-Fi power save is off while streaming, and the TCP send buffer
  is 32 KB. The camera logs frame rate, throughput and Wi-Fi signal
  strength (RSSI) every 10 s.
- **Bluetooth and video (hub)**: while a camera streams, scans for the fridge
  and the Bluetti wait up to 60 s, because scans took radio time from Wi-Fi
  and made the video erratic. Heater scans are never held back.
- **Camera logs (camera node)**: the reset reason at boot; free memory
  (internal RAM, the largest DMA block, PSRAM) at each start and stop and in
  the 10 s stats line; how long the camera took to start and how long after
  the start command the first frame went out. A failed start names the
  ESP-IDF error.
- **Cameras wake only on screen (hub)**: the Cameras page asks for a camera
  only while its tile is on screen, and lets it sleep after its tile has
  been off screen for 10 s. Before, opening the page woke every camera, and
  one scrolled past streamed to nobody for about a minute. Stop sticks: a
  camera stopped with Stop stays asleep on that device until its Watch is
  pressed, across visits to the page and reloads. The browser remembers it
  in `localStorage`, and each Stop or Watch changes only its own camera
  there, so two tabs do not overwrite each other.
- **Turned photos (hub)**: Photo saves the picture turned to the camera's
  saved rotation, as the video shows it. Before, the photo kept the
  camera's own orientation.
- **Network password required for release builds (hub, camera node)**: a
  release build stops with an error unless `M_PULSE_AP_PASS` is set,
  because the fallback password is public in this repository. Debug builds
  keep the fallback.
- **Commands only from the hub (camera node)**: on the hub's network a
  camera takes start and stop only from the hub (its gateway). On a home
  network it still takes them from any sender, and with no link it takes
  none, so an old network's hub address never carries over. A link found up
  while the camera takes nobody's commands is joined again. The README had
  said other senders were already ignored; they were not.
- **Camera registry in core, with tests**: the hub's camera list (start
  and stop wishes, retries, rotations and their flash record) moved from
  the hub to `core/src/camera.rs`, so it runs under `cargo test`. Tests
  cover Stop then Watch, wish expiry, making room for a rotation, and
  loading version 1 rotation records. No change in behavior.
- **Stop sent again (hub)**: a camera nobody asks for that still says it is
  starting or streaming gets `stop` again every 3 s, 10 times, then every
  30 s while it announces. Before, the hub sent one `stop`; if it was lost,
  or the hub restarted while a camera streamed, the camera streamed to
  nobody for 5 minutes and Bluetooth scans waited.
- **No answer stays no answer (hub)**: after 10 unanswered starts, only the
  Watch button (`start?watch=1`) tries again. Before, the page's renewals
  and wake-ups reset the count, so "No answer" flickered and the hub kept
  sending start, and the camera kept retrying its driver, for as long as
  the page was open. The page no longer wakes a camera that gave no
  answer.
- **Stalled stream ends after 10 s (camera node)**: a stream with no frame
  for 10 s of wall-clock time ends. The limit was 200 tries meant as 10 s,
  but each try can wait 4 s in the driver, so a stalled sensor kept the
  stream, and the camera, on for about 13 minutes.
- **Frame size and an open stream (camera node, hub)**: the camera refuses a
  new frame size with `409` while a viewer has the stream open, because the
  driver restart would stall that stream. The quality alone still changes
  while watching. When the camera refuses a new size, the Quality (test)
  picker closes its tile's video and tries again, up to 3 times 1 s apart,
  then opens the video again; a quality change alone does not close it.
  The picker takes one change at a time per tile.
- **Starts at most every 2 s (camera node, hub)**: the camera takes a start
  only 2 s after it last started or stopped, so a flood of commands can no
  longer cycle the driver and its memory. A stop is never held back, and a
  start whose init failed does not hold back the next one. The hub resends
  start every 3 s (was every second), the camera's 2 s gap and a second of
  margin, so each try reaches the camera even when power save delays a
  packet; both use `START_GAP` in `core/src/camera.rs`. The hub tries 10
  times (was 15), so giving up still takes about 30 s. Repeats that change
  nothing are skipped without waiting on the camera lock.

- **SHT3x cabin sensor**: the hub reads an SHT3x (SHT30/SHT31) breakout
  over I2C, with SDA on GPIO4 and SCL on GPIO5, instead of the DHT11. It
  finds the sensor at 0x44 or 0x45 by itself, checks every reading's CRC,
  and looks for the sensor again after a minute of failed reads. Readings
  are to a tenth of a degree, and the radio can no longer spoil them, so the
  DHT11's core-pinning and interrupt hold-off are gone. Wiring in
  `hub/README.md`; decoding and its tests in `core/src/sht3x.rs`. The
  DHT11 decoder is removed (the sensor node keeps its own DHT11 driver).
  Tested on the hub on 17 Nov 2026: found at 0x44, readings every 5 s with
  no failures. Mount the sensor a few cm from the hub so the ESP32's heat
  does not make it read high.

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
