# Hub (ESP32-S3)

Firmware for an ESP32-S3 board (any S3 module with 4 MB flash or more; it
uses no screen or PSRAM). The hub in the camper is a bare S3 module with
16 MB flash and no PSRAM, with the cabin SHT3x sensor wired to it. It replaced the
Freenove ESP32-S3 media kit, the first hub, whose screen was never used.
It:

- keeps a BLE link to the BougeRV diesel heater (`BougeRV-DHG001`) and
  reconnects if it drops
- keeps a second BLE link to the BougeRV Rocky 50 fridge (`SYZ-R50-...`),
  reads it every 2 s, and lets the dashboard change it (see
  [Fridge](#fridge))
- keeps a third BLE link to the Bluetti Elite 200 V2 power station and reads
  its battery, inputs and outputs every 5 s, and switches its AC and DC
  outputs when you ask on the dashboard (see [Bluetti](#bluetti))
- stops the heater cleanly when the Bluetti runs low and blocks starts until
  it has recharged (see [Heater low-battery stop](#heater-low-battery-stop),
  on after every restart, with its saved levels)
- can put the fridge in Eco while the Bluetti is low and back to Max once it
  recovers (see [Fridge saver](#fridge-saver), off until you turn it on;
  kept across a restart)
- reads the cabin temperature and humidity from its own SHT3x (see
  [Cabin sensor](#cabin-sensor)), and falls back to readings from the
  [sensor node](../sensor-node) over Wi-Fi if that sensor stops answering
- runs the thermostat on those readings
- switches the heater on and off, either from the dashboard's On and Off
  buttons or by the thermostat in Auto mode. After every restart the hub is
  in Monitor mode: the thermostat sends no switch commands until you choose
  Auto. The one command the hub can send by itself in Monitor mode is the
  low-battery stop's Off
- serves a dashboard at http://192.168.71.1 on the hub network
- sets the heater's clock from internet time (SNTP), or from your phone's
  time when you open the dashboard without internet

The heater link lives here because the classic ESP32 could not hold it: the
heater went silent about 11 s into every connection. The S3 and a Mac both
hold the same link indefinitely.

## Cabin sensor

The cabin sensor is an SHT3x (SHT30 or SHT31) on a small breakout board,
wired to the hub over I2C. It replaced the DHT11: it reads to about 0.2 °C
instead of 1-2 °C, and the S3's I2C hardware handles the bus timing, so the
Bluetooth radio can no longer spoil reads.

### Wiring

Work with the hub unplugged. Solder the supplied header to the four labelled
pads (`VIN`, `GND`, `SCL`, `SDA`). The two pads on the other side of the
board (`ADDR` and `ALERT` on most of these boards) stay unconnected.

| SHT3x board pin | Connects to on the hub |
| --- | --- |
| `VIN` | `3.3V` |
| `GND` | `GND` |
| `SCL` | `GPIO5` |
| `SDA` | `GPIO4` (the wire the DHT11's DATA used) |

- **Use 3.3V, not 5V.** The board accepts either, but its pull-up resistors
  pull SDA and SCL to whatever `VIN` is. At 3.3V the lines stay at the
  ESP32's own logic level.
- **No extra resistors.** The board has its own 10k pull-ups on SDA and SCL,
  and the hub turns on its internal ones too. Take out the 10k resistor the
  DHT11 needed between GPIO4 and 3.3V.
- **Address.** With `ADDR` unconnected the sensor answers at `0x44`; tied to
  `VIN` it answers at `0x45`. The hub tries both, so either works.
- **Placement.** Mount the sensor a few cm from the hub, or on a short lead,
  never right beside the ESP32 module: the module and its radio warm the
  air around them and the sensor reads high. Keep it away from the heater's
  outlet too, and in free air. Up to about 1 m of wire is fine at the hub's
  100 kHz bus speed. Right after soldering or flashing, the board is warm
  and the reading falls for a few minutes before it settles.
- **On a breadboard**, a wire only connects to a pin in the same numbered
  row and on the same side of the centre groove. Put each wire in the hole
  right beside its sensor pin.

The pins are set in `src/main.rs` (`peripherals.pins.gpio4` and
`peripherals.pins.gpio5`). Neither is a strapping, flash, PSRAM or USB pin
on any ESP32-S3 module.

### Checking it

After flashing, the serial log shows `SHT3x: found at 0x44` about 2 s after
boot, then `Cabin: 21.4 °C, 38 % RH (hub SHT3x)`. If it shows
`SHT3x: no sensor at 0x44 or 0x45`, the sensor did not answer at all: check
power and that SDA and SCL are not swapped. The hub keeps looking for it
every minute, so a fixed wire is picked up without a restart.

If the sensor is still not found, one wire is usually open. In the first
install, SDA was fine but the SCL jumper was broken: a multimeter in
continuity mode from the hub's `IO5` pin to the sensor's `SCL` pin should
beep, and so should `IO4` to `SDA`. Replacing the jumper fixed it.

The hub reads the sensor every 5 s. While it has a reading less than 30 s
old it ignores the [sensor node](../sensor-node), so the node is only a
backup and does not need to be running. If the hub's sensor stops answering,
the node's readings take over within about 30 s, and the dashboard adds
"sensor node" under the cabin temperature. With neither, the thermostat
sees the sensor as lost after 2 minutes and keeps the heater off.

## Networks

The hub runs its own Wi-Fi network, `M-Pulse`. The tablet and your phone
(and the sensor node, if you still run it) join it, so everything works in
the camper with no router. This is the tested setup.

To set your own password for that network, put it in `wifi.env` at the top of
the repository. Git ignores this file, so the password stays on your computer:

```sh
export M_PULSE_AP_PASS="password for the M-Pulse network"   # 8+ characters
```

A release build (`cargo build --release`) stops with an error without it,
because the fallback password, `mpulse-heater`, is public in this
repository. A debug build (no `--release`) still uses that fallback, for the
bench. Flash the cameras and the sensor node from the same `wifi.env`, or
they cannot join. If a board ever ran with the fallback, change the
password: anyone who has read this repository could join.

### Joining another Wi-Fi (optional, untested)

The firmware can also join an existing network, such as your home Wi-Fi, for
internet time and to show the dashboard there. It is built in but has not been
tested, and it has trade-offs:

- While the hub is on another network, its own network moves to that
  network's channel, so the tablet, phones and the sensor node may drop off
  briefly when it joins.
- If the sensor node joins that network too and the hub is not on it, its
  backup readings stop reaching the hub. This only matters if the hub's own
  sensor has failed.

To try it, add these lines to `wifi.env` and rebuild the hub (and the
sensor node, if you use it):

```sh
export M_PULSE_WIFI_SSID="your network"
export M_PULSE_WIFI_PASS="your password"
```

Without internet time the dashboard sets the hub's clock from your phone.

## Opening the dashboard on a phone

Join the `M-Pulse` Wi-Fi and open `http://192.168.71.1` (type the `http://`).

- **Android 17 and later** block apps from reaching local devices by default.
  If Chrome shows `ERR_TOO_MANY_RETRIES`, allow it in Settings > Apps >
  Chrome > Permissions > Local network.
- If the page does not load at all, turn mobile data off or tell the phone to
  stay connected when it warns that the network has no internet.

## Using the dashboard

The page is built for the camper's tablet and works on a phone and a laptop
too. It opens on **Home**, a summary with one tile per service, and each
service has its own page: **Climate** (heater and fuel), **Fridge**,
**Power** and **Rules**. Tap a tile, or use the tab bar (at the bottom on a
phone or tablet, down the left side on a laptop with a mouse). Each page has
its own address (`#home`, `#climate`, `#fridge`, `#power`, `#rules`), so a
bookmark opens it directly. After 5 minutes without a touch the screen goes
back to Home. Everything is inside the one page the hub serves, so it needs
no internet.

- **Home**: the Climate tile shows the cabin temperature, the heater's state
  and the mode, with **−** and **+** for the target, so the heater is one
  tap away. The Fridge, Power, Fuel and Rules tiles show the headline values
  and turn red or amber with their alerts. On a tablet or laptop the tiles
  fill the screen and show a few more values (solar, charger and outputs;
  fridge power and input; burn rate and last fill-up; each rule's state).
  On a phone it all fits on one screen.

Each card has a small (?) button in its header that opens help for that
card: what each value and control means and how the automation behaves.
The footer's *Alerts and screen help* explains the alerts, top bar and
themes.

- **Top bar**: a chip for each appliance with its link (green when
  connected, amber while connecting, red while searching) and its headline
  value: what the heater is doing, the temperature inside the fridge and the
  Bluetti's charge. A chip turns red with that appliance's alarm (heater
  fault, warm fridge, charge at 20 % or below). Tap one to open its page. The logo pulses each time fresh data arrives. The buttons on the
  right change the theme (dark, a dim red *Night* theme for after dark, and
  light; each device remembers its choice) and go full screen.
- **Alerts** appear under the top bar. Home lists all of them, one line
  each, and tapping one opens its page, which shows it in full. Each page
  shows only its own, and a tab gets a red or amber dot while its page has
  one. The alerts: the low-battery stop
  holding the heater off, low fuel or the fuel reserve, a heater fault code, the Bluetti at 20 % or below,
  the fridge warm alert, a heater that did not start
  after 3 tries, a cabin sensor that is missing or more than 2 minutes old,
  and a hub that has not answered for 12 s. When the hub cannot be reached,
  the cards also fade until it answers again.
- **Climate**:
  - The dial shows the cabin temperature against a 5 to 30 °C scale, with
    the target as a knob and the comfort band (target ± 1 °C by default,
    where the thermostat holds its state) as a lighter stretch of track. Humidity and
    the reading's age sit in the middle.
  - *Cabin, last 4 h* charts the cabin temperature against the comfort
    band, with a strip along the bottom for the minutes the heater was
    burning. Touch or hover for the values at any minute. The hub keeps
    one point a minute in memory, so the chart starts empty after a
    restart and fills over four hours.
  - **+** and **−** change the target in 0.5 °C steps. Hold a button to
    keep stepping. Quick taps add up and go to the hub as one change; the
    target shows amber until the hub has it.
  - *Comfort band*, under the target, sets how far the cabin may drift
    before the thermostat acts: **−** and **+** in 0.5 °C steps from ±0.5 °C
    to ±3 °C (`POST /api/band`), with a line saying where heat starts and
    stops. Taps add up like the target's. The band is saved in flash, so a
    restart keeps it. A narrower band starts the heater more often, but the
    10-minute minimum run and rest still hold, so never more than one start
    every 20 minutes.
  - The heater panel shows what the heater is doing (Off, Starting,
    Igniting, Running, Cooling down, Fault or Searching) with an animated
    flame, and which of its parts (flame, glow plug, fuel pump, fan) are on.
    *Igniting* means it has accepted a start but is not yet burning steadily
    (that takes about 4 to 5 minutes); *Running* means it has a flame and the
    glow plug is off.
  - While the heater burns, the power panel has a *Burning 25 min* badge
    and the heater card says how long it has been burning
    (counted by the hub from when it first sees a steady flame; a dropped
    Bluetooth link does not restart the count, but a hub restart does, and
    then it reads *over* 25 min because the heater lit before the hub was
    watching).
  - The power panel mirrors the heater's new LCD remote: the power setting
    in kW with a colour bar, the supply voltage, and **+** / **−** that
    change the setting in 0.1 kW steps (hold to keep stepping; quick taps go out as one change). The
    setting shows amber until the heater reports it back. The hub allows
    1.0 to 5.0 kW; the heater's own limits are not known yet, so a toast
    says so if the heater keeps a different value. Changing the power never
    switches the heater on or off and leaves Auto / Monitor alone, so the
    thermostat's glow-plug protections (minimum run and rest times,
    deadband) are untouched. Below it is the thermostat's reason in plain
    words.
  - *Monitor only* (the mode after every restart): the thermostat decides
    but does not switch the heater. *Auto* (asks first): the thermostat
    switches the heater. Pressing Auto again also retries after 3 failed
    starts.
  - **Heater ON** (asks first) and **Heater OFF** switch the heater now. Any
    manual press puts the hub back in Monitor only, so the thermostat does
    not undo it. Heater ON is refused while the heater reports a fault, is
    not fully off, or the low-battery stop is holding it off, so the button
    is greyed out then, with the reason under it. A refused press leaves
    Auto in charge.
  - *Heater details* opens the raw readings: link, state, parts, power,
    the heater's own set temperature, supply voltage, whether the hub has
    set the heater's clock, the last power change, error code, firmware,
    last command, and the raw 20-byte status report. The raw bytes are
    there to find the altitude and combustion efficiency the LCD remote
    shows, which the hub cannot decode yet (see below).
- **Heater fuel**: the estimated litres and percent left, a tank bar with
  the warning and reserve marks, hours of burning left at the current power,
  and **Tank full** with an optional "litres added" box. It sits on the
  Climate page. *Fuel settings* holds the level and tank size by hand and
  the learned burn rate; the warning and reserve levels are on the Rules
  page. See
  [Heater fuel estimate](#heater-fuel-estimate).
- **Fridge** shows the temperature inside in a ring that frosts over while
  the fridge runs and turns red with the warm alert (more than 4 °C above its
  target for 30 minutes: check the lid and the power). **+** and **−**
  change the target by 1 degree, and quick taps go out as one change;
  **Eco** / **Max** change the mode; the **Power** switch turns it on, or off
  after asking. *Fridge details* has the input voltage, battery protection
  level, panel lock and last command.
- **Power** shows the Bluetti: charge, time to full or empty when the
  station has an estimate (otherwise the net watts), and a flow picture with
  solar and charger input on the left and AC and DC output on the right. The
  lines animate while power flows, and the battery's colour follows its
  charge (green, amber at 50 % and below, red at 20 % and below). Under it,
  the **AC outlets** and **DC outputs** switches turn those outputs on, or
  off after asking. A switch shows the new position straight away and
  settles on what the Bluetti reports within a few seconds. The hub refuses
  some Offs (see [Bluetti output switches](#bluetti-output-switches)); the
  reason shows in a note at the bottom of the screen. *Battery details* has
  the voltage and current, and the last switch command and whether the
  Bluetti took it. *Charge, last 4 h* charts the charge, with the heater's
  low-battery stop level marked.
- **Rules**:
  - *Heater low-battery stop*: a switch (turning it off asks first), the
    stop and restart levels, and what it is doing now.
  - *Fridge saver*: a switch, the Eco and Max levels, what it is doing now
    and its last change.
  - *Heater fuel reserve*: whether the reserve stops the heater, the
    warning and reserve levels, and what it is doing now.

  Levels change in 5 % steps with **+** and **−** and go to the hub a
  moment after you stop tapping. Raising one level past the other moves the
  other too, to keep the 5 % gap the hub needs. The hub keeps them in flash
  across a restart or power cut (the fuel levels with the fuel estimate).
- **Footer** (Home only): the hub's clock and its address on another Wi-Fi, if joined.
  The page sets the hub's clock from the device's time when the hub has no
  internet time.

The Climate, Fridge and Power cards show `--` in place of readings while
their link is down, never the last values.

## Heater LCD remote features

BougeRV's new LCD remote for the diesel heater shows 15 things. What the hub
does with each, against the DHG001's BLE protocol:

| Remote | Hub | From |
|---|---|---|
| On/Off | Heater ON / OFF, and Auto | command `0x05`, tested |
| Fuel pump, air blower, ignition electrode, heater core | Pump, Fan, Glow and Flame tiles | run byte bits, tested |
| Heater status | Off / Starting / Igniting / Running / Cooling down / Fault | run byte, tested |
| Power level (kW) and heating power bar | Power panel: the power setting and the colour bar | status byte 12, tested |
| + and − | Power setting + / − | command `0x07` (kW × 10), tested on the hub 14 Nov 2026 (1.4 → 1.5 kW read back) |
| Voltage | Badge in the power panel | status byte 14 × 0.2 V, tested while burning (reads 0 while idle) |
| Clock | The hub sets the heater's clock on every connection | command `0x0A`, tested |
| Timer (bell next to the clock) | Not shown. The dashboard shows how long the heater has been burning instead | status byte 11 bit 1 is thought to be the timer flag, but it was on in every report, so it is **not confirmed** |
| Heating mode (MANUAL) | Auto / Monitor only: the hub's thermostat replaces the heater's own modes | the heater's own mode byte is **not decoded** |
| Altitude | Not shown | **not reported over BLE**: every spare status byte stayed 0 through a burn |
| EFF (combustion efficiency) | Not shown | **not reported over BLE**, same check |
| SET | Not needed: settings live on the dashboard | |

Burn test, 14 Nov 2026 (DHG001, heater firmware 1.1.2, 2½ minutes of
reports from start to steady flame):

- Status byte 5, which the vendor app calls live power, is not kW: it
  climbed from 0.8 to 3.1 with the setting at 1.8 kW. It is shown only in
  *Heater details*, as "Byte 5".
- Bytes 7 to 10 and 15 to 18 stayed 0, so altitude and efficiency are not
  in the report. The remote probably works them out itself.
- Byte 4 moved between 67 and 124 while burning; maybe fan or pump speed.

Still open: the heater's real power limits (the hub allows 1.0 to 5.0 kW;
change `POWER_MIN_X10` / `POWER_MAX_X10` in `core/src/protocol.rs` once
they are known).

## Fridge

The Rocky 50 speaks the protocol of Alpicool-family compressor fridges,
read from the BougeRV app (v2.6.2) and checked on a live fridge. The wire
format and the warm alert live in [`core/src/fridge.rs`](../core/src/fridge.rs).

- Service `00001234-0000-1000-8000-00805f9b34fb`; commands are written to
  `...1235...` (write without response), replies arrive on `...1236...`
  (notify). The subscription must be written with a response, or the fridge
  never replies.
- Frames are `FE FE len cmd payload.. sum_hi sum_lo`. The hub sends the
  status query `FE FE 03 01 02 00` every 2 s; the fridge never reports
  unasked.
- Target changes use command `05`. Power and Eco/Max rewrite the whole
  settings block (command `02`) from the last status, changing one field, so
  the hub sends them only with a status less than 10 s old.
- The heater, fridge and Bluetti links take turns to search and connect,
  because NimBLE allows one scan or connection attempt at a time. All three
  are then held together (`CONFIG_BT_NIMBLE_MAX_CONNECTIONS=4`). A link
  that finds nothing hands the turn on before its 10 s pause.
- Searches listen 30 % of the time (30 ms in every 100 ms). Wi-Fi and
  Bluetooth share one radio, and a search that listened all the time left
  the dashboard stalling for seconds while a device was out of range.
- While the hub holds the link, the BougeRV phone app cannot connect to the
  fridge. Unplug the hub to use the app.

## Bluetti

The Elite 200 V2 speaks Modbus over BLE inside an encrypted channel, like
every newer Bluetti. The key exchange comes from the open-source
`bluetti-bt-lib`; the register map from the Bluetti app (v3.1.5). Both live
in [`core/src/bluetti.rs`](../core/src/bluetti.rs). No Bluetti account or
licence file is needed.

- Service `ff00`; requests are written to `ff02` (with response), replies
  and key-exchange messages arrive on `ff01`. The hub finds the station by
  that service; it advertises no name.
- On connect the station sends a challenge, then its signed P-256 key. The
  hub answers with its own signed key, and from then on every message is
  AES-256-CBC under the shared secret. The station repeats its key until it
  hears back, and the hub ignores the repeats.
- The hub reads 62 registers from register 100 (the app's home screen)
  every 5 s: charge %, battery voltage and current, solar and charger
  input, AC and DC output, and whether the AC and DC outputs are on. Four
  missed replies in a row close the link and rebuild it.
- Battery voltage is in hundredths of a volt on this model (the app divides
  by 10). The station reports 5994 minutes to full or empty when it has no
  estimate, and the dashboard hides that.
- While the hub holds the link, the Bluetti app can only reach the station
  over the cloud, not Bluetooth.

### Bluetti output switches

The dashboard's AC and DC switches write 1 (on) or 0 (off) to register 2011
(AC) or 2012 (DC), as one Modbus "write single register" inside the
encrypted channel. The station echoes the request back, and the hub reads
the home block straight after, so the dashboard shows the result within a
couple of seconds. The wire format is in
[`core/src/bluetti.rs`](../core/src/bluetti.rs).

- The addresses are the ones `bluetti-bt-lib` uses for the Elite 30 V2 and
  Elite 100 V2, the same protocol family. They work on the Elite 200 V2
  too (see [Tested](#tested)).
- A command is sent once and never retried. It is dropped if the link goes
  down before it is sent.
- The hub only sends one with a Bluetti reading under 30 s old.
- **DC off is refused while the heater is starting, running or cooling
  down.** If the heater runs from the Bluetti's 12 V output, cutting it
  mid-cycle skips the cool-down that protects the glow plug.
- To make sure an output that something important runs from (the hub, the
  fridge, the tablet) is never switched off from the dashboard, list it at
  build time in `wifi.env` and reflash:

  ```sh
  export M_PULSE_BLUETTI_KEEP_ON="dc"   # or "ac", or "ac,dc"
  ```

  That output can still be switched on, and its switch shows dimmed.

## Heater low-battery stop

When the Bluetti's charge falls to the stop level (default 15 %), the hub
sends the heater its normal Off, so it runs its full cool-down and the glow
plug is cleared properly, and refuses to start it (thermostat and the
dashboard's On button). Starts are allowed again once the charge is back to
the resume level (default 25 %); in Auto the thermostat may then restart the
heater, in Monitor it stays off until switched on. The logic is in
[`core/src/heater_cutoff.rs`](../core/src/heater_cutoff.rs).

- It is on after every restart: a safety, so turning it off lasts only
  until the hub restarts. The levels, and an off switch, are on the
  dashboard's Rules page; the levels are kept across a restart; a red alert shows at the top of the page
  while it holds the heater off.
- A heater that is already cooling down or off is left alone. The Off is
  repeated every 20 s while the heater still reports On or Starting.
- A lost Bluetti link neither trips it nor clears it. It needs a Bluetti
  reading under 30 s old to change state.
- It works in Monitor mode too. A start that was waiting to be sent when it
  trips is dropped.
- The stop level can be 5 to 50 %, and the resume level at least 5 points
  above it.

## Heater fuel estimate

The heater does not report its fuel level, so the hub works it out from how
long the heater burns and at what power. No sensor is needed. The logic is in
[`core/src/fuel.rs`](../core/src/fuel.rs).

- **Start it**: fill the tank to the top and tap **Tank full** on the
  dashboard's Heater fuel card. Until then the level shows `--` and neither
  the warning nor the reserve does anything.
- **Burn rate**: from BougeRV's manual for the 5 kW heater, 0.18 L/h at
  1.4 kW up to 0.53 L/h at 5.0 kW, in a straight line between them. Fuel is
  counted only while the heater reports Starting or On with its pump or
  flame on, so the glow plug warm-up and the cool-down are not counted.
- **It learns your heater**: at each fill to the top, type how many litres
  went in before tapping Tank full. If the hub counted every burn since the
  last fill to the top (and at least 1 L of it), it compares the two and
  moves its rate halfway toward what the fill-up measured. "Rate vs the
  manual" under *Fuel settings* shows the result. A ratio outside 0.5 to 2×
  is treated as a typing slip and ignored.
- **Warning** (default 15 %): an orange alert and a Low badge.
- **Reserve** (default 15 %): the hub sends the heater its normal Off, so it
  runs its full cool-down, and refuses starts from Auto and from the On
  button until you refill and tap Tank full or set a higher level. The
  reserve can be set to only warn (*Reserve stops the heater* switch).
- Both levels can be 5 to 50 % in 5 % steps. The tank size (default 5 L, 1
  to 60 L) and the level can be set by hand under *Fuel settings*. Setting
  the level by hand means the next fill-up cannot correct the rate.
- **Saved in flash** (two copies with a checksum, NVS namespace `fuel`): the
  level, the learned rate and all the settings survive a power cut. The level
  is written about every 0.02 L while burning.

What it cannot see:

- Burning while the hub is off or out of range. If the link drops while the
  heater burns, the hub keeps counting at the last power for up to an hour
  (so it reads low rather than high) and marks the estimate **Uncertain**.
  A heater found already burning after a restart or reconnect is marked
  Uncertain too. A whole burn started and stopped from the LCD remote while
  the hub was off leaves no trace. Uncertain clears at the next fill-up.
- Accuracy is an estimate, not a measurement: roughly ±15 to 20 % on the
  manual's figures, better after a few fill-ups with the litres entered.
  Altitude (the heater trims fuel up high), battery voltage and cold fuel
  all move the real rate.

## Fridge saver

Switches the fridge to Eco when the Bluetti's charge falls to the low level,
and back to Max when it is up to the high level again (defaults 30 % and
50 %). The logic is in [`core/src/eco_guard.rs`](../core/src/eco_guard.rs).

- It is off until you turn it on and pick the levels on the dashboard's
  Rules page. Both are kept across a restart.
- It acts only when the charge crosses a level, and restores Max only if it
  was the one that set Eco. Change the mode by hand and it leaves it alone.
- It needs a Bluetti reading under 30 s old and a fridge status under 10 s
  old; without them it waits. A change the fridge does not take is retried
  every 30 s, three times at most.
- The low level can be 5 to 95 %, and the high level at least 5 points above
  it.

## Cameras

The Cameras page (the last tab) shows one tile per camera on the hub's
network: its name, whether it is streaming, starting, asleep or offline, and
its video while it streams. A camera appears once it has announced itself.
The Home page has a Cameras tile that shows how many are online.

- Each camera announces its name and state to the hub every 5 s on UDP port
  4220, and the hub takes the camera's address from that packet. A camera
  with no announcement for 15 s is offline.
- Cameras sleep until someone asks. On the Cameras page, a camera is asked
  for while its tile is on screen: opening the page asks for the cameras
  you can see, and scrolling a tile into view asks for that one. A tile off
  screen for 10 s lets its camera sleep, so on a phone the camera you
  scrolled past does not keep streaming. Stop sticks: a camera you stopped
  stays asleep on that device until you press its Watch, even after you
  leave the Cameras page, close the browser or reload. The browser keeps
  the list (`localStorage`, key `mpulse-cam-stopped`), so each phone or
  tablet has its own, and clearing the browser's site data forgets it.
  Tabs of the same browser share it: a Stop or Watch in one tab does not
  undo another tab's.
  Each tile has four buttons and a picker:
  - **Watch** asks the hub to start that camera.
  - **Rotate** turns the picture 90 degrees each time you press it and saves
    the angle on the hub, so every device shows the same picture.
  - **Photo** saves a still JPEG of the camera while it streams, turned to
    the camera's rotation like the video. A turned photo is encoded again
    by the browser (JPEG quality 0.92); at 0 degrees it is the camera's own
    file.
  - **Stop** asks the hub to stop that camera.
  - **Quality (test)** sets the camera's frame size and JPEG quality until
    the camera restarts. The default is SVGA (800 by 600), quality 12. This
    picker is for comparing settings and may be removed. A quality change
    applies at once. The camera refuses a new frame size while anyone
    watches, this tile included, so the tile then closes its own video and
    tries again, up to 3 times 1 s apart, and opens the video again after.
    If another device is watching, the tile says the change was refused.
    The picker is greyed out until a change and its retries are done.
- Leaving the page, switching to another browser tab, or closing the browser
  asks the hub to stop the cameras that page had started. While the page is
  open and the tile is on screen, the page renews the request every 20 s,
  even while the video is paused, so the video plays until you press Stop or
  leave. A request lapses 60 s after its last renewal, so a closed browser
  cannot keep a camera on for more than a minute.
- As a backup, a camera that streams with no viewer for 5 minutes goes back
  to standby on its own. The hub ends that request, and a tile still on
  screen asks for the camera again within 5 s.
- If the picture stops for 10 s (Wi-Fi lost, phone asleep), the tile says
  "Video paused" and reconnects when it can.
- Waking is quick: the hub sends `start` or `stop` the moment the page asks
  (not at its next once-a-second check), the camera announces each change of
  state at once, and the page checks every 0.5 s (instead of 2 s) while a
  camera on screen is waking, starting from the moment it asks for it.
- The hub sends `start` every 3 s until the camera says it is streaming, for
  10 tries (about 30 s) at most. The camera takes a start only 2 s after it
  last started or stopped, so a second of margin lets each try reach it,
  even when Wi-Fi power save delays a packet. After that the tile says "No
  answer from the camera" and the page stops asking. Press Stop, then
  Watch, to try again: only the Watch button starts a fresh round of 10
  tries, not the page's renewals.
- The hub sends `stop` when a request ends. If a camera nobody asks for
  still says it is starting or streaming (the stop was lost, or the hub
  restarted while it streamed), the hub sends `stop` again every 3 s, 10
  times, then every 30 s, for as long as the camera keeps announcing.
- The video is the camera's own stream on port 81, loaded only while its
  tile is on screen. One viewer can watch a camera at a time; a second viewer
  waits until the first stops. Each time the tile opens the video it adds a
  new `?n=` to the address, so after the camera restarts the browser connects
  again instead of showing the last frame of the old stream.
- While a camera streams, the hub holds back Bluetooth scans for the fridge
  and the Bluetti for up to 60 s, because a scan takes radio time from
  Wi-Fi. Scans for the heater are never held back, since the thermostat and
  the low-battery stop depend on that link.
- Measured on the bench at SVGA quality 12, one camera at a time (medians,
  November 2026): the cab board about 13 frames a second (about 200 KB/s),
  the camper board about 7 on a weaker link; both were about 5 and 3.5
  before the camera firmware's frame-rate changes. The rate swings from 1
  to over 25 within minutes, with the hub's Bluetooth and the shared air.
  Boards differ: one earlier cab board managed only 1 to 2 frames a second
  with a strong signal, and was replaced. Two cameras streaming at once
  share the hub's radio and each gets less. Not yet measured in the camper.
- Names are set when the camera firmware is built, with
  `M_PULSE_CAMERA_NAME` (see [camera-node](../camera-node/README.md)). The
  default is `camper`. The second camera is `cab`.
- The API is `GET /api/cameras` (a JSON array with name, ip, state, online,
  wanted, rotation, note, stream_url and still_url), and `POST
  /api/cameras/<name>/start` or `/stop`, which return one entry. An unknown
  name gets an error. `start?watch=1` is the Watch button: after a no answer
  it starts a fresh round of tries, which a plain `start` (a renewal) does
  not.
- The rotation (0, 90, 180 or 270 degrees, default 0) is kept per camera
  name in flash, in the same two-copy store as the thermostat settings, and
  written only when it changes. `POST /api/cameras/<name>/rotation` takes the
  number as plain text (`90`) and returns the entry; any other value is a 400.
- The access point takes up to 8 connections (it was 4), for the phone, the
  tablet, the sensor node and two cameras.

The messages and the hub's registry (when to send start and stop, and the
saved rotations) are in [`core/src/camera.rs`](../core/src/camera.rs), with
tests that run with `cargo test` in `core/`. The hub's UDP side is in
`src/camera.rs`.

Tested: a second camera board with an OV2640 (sensor ID 0x0026) also works.
The camper's camera (Freenove WROVER with an OV3660, sensor ID 0x3660) still
needs a test with the current hub.

Not measured: the standby current. The WROVER board has no power-down pin,
so standby only stops the camera clock and the sensor stays powered. See
[camera-node](../camera-node/README.md#power).

## Build and flash

Set up the toolchain first: [../docs/mac-setup.md](../docs/mac-setup.md).

A release build needs `M_PULSE_AP_PASS` from `wifi.env` (see
[Networks](#networks)); without it the build stops with an error.

```sh
source ../wifi.env
cargo build --release
espflash flash --monitor --partition-table partitions.csv target/xtensa-esp32s3-espidf/release/m-pulse-hub
```

With more than one board or phone plugged into the Mac, espflash asks which
port to use. The hub is the one listed as `USB Single Serial` (on this Mac,
`/dev/cu.usbmodem5C940471791`); pass it with `--port` to skip the question.
The dashboard page is built into the firmware, so changing it means
reflashing the hub. After a flash the hub is back in Monitor only.

To work on the dashboard without the hub, run the mock hub, which serves
`src/dashboard.html` with made-up readings and answers every route the page
uses (listed at the top of [`src/web.rs`](src/web.rs)):

```sh
python3 tools/mock_hub.py              # http://127.0.0.1:8700, heater burning
python3 tools/mock_hub.py --scenario alerts   # low battery, fault, warm fridge
```

## Tested

The heater, fridge, Bluetti and dashboard tests below were run on the first
hub, the Freenove ESP32-S3 media kit. The current hub is a different S3
module running the same firmware; so far only its cabin sensor and the
dashboard server have been checked (see the end of this section).

Tested on the BougeRV DHG001 heater:

- Manual ON and OFF from the dashboard, including a full start (flame after
  about 100 s, steady after about 270 s) and the 4 to 5 minute cool-down.
- Auto: raising the setpoint started the heater within a second; lowering it
  held the heater on until the 10 minute minimum on time, then switched it
  off. Only those two commands were sent, and the link did not drop.

Tested on the BougeRV Rocky 50 fridge, alongside the heater link:

- The hub reads the fridge every 2 s, and both links held together.
- From the dashboard: target up to 7 °C and back to 4 °C, Eco then Max, and
  Off then On. The fridge applied each change within about a second.

Tested on the Bluetti Elite 200 V2, alongside the heater and fridge links:

- The key exchange finishes about 1.3 s after connecting, and the station
  answers every read within 0.1 s. All three links held together.
- First read: model `Elite200V2`, 94 %, AC and DC outputs on, no load and no
  input.
- With an AC load: 39.66 V at 2.2 A out matched the 83 W the station
  reported, and the charging-status code read 2 (discharging).

Tested for the fridge saver, on the real fridge and Bluetti (charge 91 %):

- With the levels set to 95/100 % the fridge went to Eco within 2 s; moving
  them to 5/10 % put it back to Max.

Tested for the dashboard (the redesign in PR #10):

- In a browser against a stand-in hub with the same API: landscape tablet
  (1280×800), portrait tablet (800×1280) and phone (390×844), in the dark,
  Night and light themes, with no sideways scrolling. Every control sent the
  expected single request, and cancelling a confirmation sent nothing. The
  alerts, the links-down state and an unreachable hub all showed correctly,
  and the page recovered by itself when the hub came back.
- Flashed to the hub: it booted, started the dashboard server and
  reconnected the fridge. The 1.5 MB firmware uses about half of the 3 MB
  app partition.

Tested for the heater low-battery stop:

- With the stop level set above the charge it tripped within a second, and
  it cleared when the levels were lowered.

Tested for the Bluetti output switches (PR #16), on the current hub:

- From the dashboard, the AC and DC switches both worked: the Elite 200 V2
  followed them.

Tested for the heater fuel estimate (PR #23), on the current hub with the
heater, fridge and Bluetti connected (15 Nov 2026):

- Tank full started the gauge, and a burn took fuel off it.
- Setting the level at the reserve while the heater burned sent the normal
  Off and blocked the On button until the level was raised.
- The level and settings came back after a hub restart.

Tested for v1.0.0 (PR #21), on the current hub:

- CI built the hub and sensor-node firmware with no Clippy warnings, and the
  core tests (76) passed, including the new ones for the thermostat taking
  over a running heater, the DHT11 decoder and the trend history.
- The dashboard in a browser against `tools/mock_hub.py`: chips, greyed-out
  heater buttons, the trend tooltip, one setpoint request per burst of taps,
  the fridge mode hold, the three themes and the links-down state, with no
  page errors.
- Flashed on 15 Nov 2026 (1.58 MB, about half the app partition): booted,
  started the network and dashboard server within a second, and read the
  DHT11 to a tenth of a degree (22.1 °C, 28 % RH). The heater, fridge and
  Bluetti were out of range, so no link was made.

Tested for the SHT3x cabin sensor (PR #28), on the current hub (17 Nov 2026):

- Flashed from main (1.67 MB, about half the app partition). The hub found
  the sensor at 0x44 about 3 s after boot and read it every 5 s with no
  failed reads: 24.4 °C and 31 % RH at first, easing to 22.8 °C and 33 % RH
  as the freshly soldered board cooled.
- The thermostat left `SensorLost` and reported `WarmEnough` above the
  18 °C setpoint. The Bluetti link held alongside it.
- With the SCL wire open, the hub logged `no sensor at 0x44 or 0x45`, kept
  the heater off, and kept running.

Tested on the current hub (bare S3 module, PR #13):

- Booted, started the dashboard server, and received the sensor node's
  readings before the DHT11 was moved over.
- With the DHT11 on GPIO4: first reading 2 s after boot (22 °C, 21 % RH),
  then fresh readings every few seconds, and the thermostat acted on them.

Not yet tested:

- **The heater and fridge links on the current hub.** They were out of
  range when it was flashed. The Bluetti link works on it. This also checks the shorter
  Bluetooth search time from PR #12, which has not yet been tried with the
  devices switched on.
- Handing over to the sensor node when the hub's sensor stops answering.
- **Stopping a burning heater with the low-battery stop.** The heater was
  off in the test above, so no Off was sent. The Off command itself is the
  one the dashboard's Heater OFF button sends, which is tested.
- Solar and charger input readings, and the sign of the charger reading.
  The Bluetti has only been read idle and with an AC load.
- The fridge saver and the heater stop at a real low charge. Both were
  triggered by moving their levels, not by draining the Bluetti.
- Joining another Wi-Fi (see [Networks](#networks)).
- The dashboard on the real tablet and phone, with live data, including the
  trend charts over a full four hours.
- The v1.0.0 fixes against the real appliances: Auto taking over a heater
  started by hand (it should keep burning inside the comfort band), a quick
  fridge Off then Eco or Max (both should stick), and the Bluetti AC/DC
  switches still reported as taken.

## Thermostat defaults

| Setting | Value |
| --- | --- |
| Setpoint | 18 °C (change it on the dashboard) |
| Comfort band | ±1 °C (change it on the dashboard, ±0.5 to ±3 °C; kept in flash) |
| Heat on | at or below setpoint − band |
| Heat off | at or above setpoint + band |
| Minimum on time | 10 minutes |
| Minimum off time | 10 minutes |
| Sensor timeout | 2 minutes without a reading turns heat off |

They live in `Config::default()` in `../core/src/thermostat.rs`. A setpoint
change from the dashboard lasts until the hub restarts; a comfort band
change is saved in flash (`thermostat::Saved`) and survives it.

## Low-battery defaults

| Setting | Default | After a restart |
| --- | --- | --- |
| Heater stop | at or below 15 % | always on, saved levels |
| Heater starts allowed again | at or above 25 % | |
| Fridge to Eco | at or below 30 % | off until turned on, then saved |
| Fridge back to Max | at or above 50 % | |

They live in `Config::default()` in `../core/src/heater_cutoff.rs` and
`../core/src/eco_guard.rs`. Changes from the dashboard are saved in the
`rules` namespace of the `nvs` partition (two copies, like the fuel gauge;
see `../core/src/rules.rs`). Erasing the flash brings back the defaults.

## Safety notes

- The heater has no idea the hub exists. If the hub loses power or its
  Bluetooth link while the heater is running, **the heater keeps running** at
  its own setting until you switch it off at the heater or its remote. The
  low-battery stop cannot help then either.
- The low-battery stop needs the hub's links to both the Bluetti and the
  heater. It has not yet been tried on a burning heater (see
  [Tested](#tested)), so do not rely on it alone to protect the Bluetti.
- The hub switches the Bluetti's AC or DC outputs only when you press a
  switch on the dashboard. Nothing automatic does it. Turning one off asks
  first, and DC off is refused while the heater is not fully off. Use
  `M_PULSE_BLUETTI_KEEP_ON` for any output the hub itself runs from.
- In Auto mode, a lost sensor (no reading for 2 minutes) switches the heater
  off once it is burning steadily.
- The hub never starts a heater that reports a fault or is still in its
  shutdown cycle, and never stops one in the middle of its start-up unless
  the start has run for more than 10 minutes.
- Auto waits the minimum on and off times after any switch, including one
  made with the manual buttons, and gives up after 3 start attempts.
- Anyone on the `M-Pulse` network can use the dashboard. Set your own
  `M_PULSE_AP_PASS` (release builds require it).
- The hub only sends a command while it has a fresh status from the heater
  (under 10 seconds old).
- The hub restarts in Monitor mode. After a power cut it will not resume
  Auto by itself. The low-battery stop always comes back on. Its levels,
  the fridge saver's levels and switch, and the fuel estimate and its
  settings are kept.
- The fuel reserve is only as good as the estimate (see
  [Heater fuel estimate](#heater-fuel-estimate)). Check the tank by eye now
  and then, and correct the level when it is off.
