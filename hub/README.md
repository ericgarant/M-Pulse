# Hub (ESP32-S3)

Firmware for an ESP32-S3 board (any S3 module with 4 MB flash or more; it
uses no screen or PSRAM). The hub in the camper is a bare S3 module with
16 MB flash and no PSRAM, with the cabin DHT11 wired to it. It replaced the
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
  on after every restart)
- can put the fridge in Eco while the Bluetti is low and back to Max once it
  recovers (see [Fridge saver](#fridge-saver), off after every restart)
- reads the cabin temperature and humidity from its own DHT11 (see
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

The DHT11 is wired to the hub. Hold it with the grille facing you and the
pins pointing down; pins are numbered 1 to 4 from left to right.

| DHT11 pin | Connects to |
| --- | --- |
| 1 (VCC) | `3.3V` on the board |
| 2 (DATA) | `GPIO4`, plus a 10k resistor to `3.3V` |
| 3 | not connected |
| 4 (GND) | `GND` |

The data pin is set in `src/main.rs` (`peripherals.pins.gpio4`). GPIO4 is
free on every ESP32-S3 module: it is not a strapping, flash, PSRAM or USB pin.

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

Without it the password is `mpulse-heater`. If you run the sensor node,
flash it from the same `wifi.env`, or it cannot join.

### Joining another Wi-Fi (optional, untested)

The firmware can also join an existing network, such as your home Wi-Fi, for
internet time and to show the dashboard there. It is built in but has not been
tested, and it has trade-offs:

- While the hub is on another network, its own network moves to that
  network's channel, so the tablet, phones and the sensor node may drop off
  briefly when it joins.
- If the sensor node joins that network too and the hub is not on it, its
  backup readings stop reaching the hub. This only matters if the hub's own
  DHT11 has failed.

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

The page is built for the camper's tablet and works on a phone too: on a
tablet the cards sit side by side, and on a phone they stack, with a tab bar
at the bottom to jump between them. Everything is inside the one page the
hub serves, so it needs no internet.

- **Top bar**: a dot for each link (heater, fridge, Bluetti): green when
  connected, amber while connecting, red while searching. Tap one to jump to
  its card. The logo pulses each time fresh data arrives. The buttons on the
  right change the theme (dark, a dim red *Night* theme for after dark, and
  light; each device remembers its choice) and go full screen.
- **Alerts** appear in a strip under the top bar: the low-battery stop
  holding the heater off, the fridge warm alert, a heater that did not start
  after 3 tries, a cabin sensor that is missing or more than 2 minutes old,
  and a hub that has not answered for 12 s. When the hub cannot be reached,
  the cards also fade until it answers again.
- **Climate**:
  - The dial shows the cabin temperature against a 5 to 30 °C scale, with
    the target as a knob and the comfort band (target ± 1 °C, where the
    thermostat holds its state) as a lighter stretch of track. Humidity and
    the reading's age sit in the middle.
  - **+** and **−** change the target in 0.5 °C steps. Hold a button to
    keep stepping. Quick taps add up and go to the hub as one change; the
    target shows amber until the hub has it.
  - The heater panel shows what the heater is doing (Off, Starting,
    Igniting, Running, Cooling down, Fault or Searching) with an animated
    flame, and which of its parts (flame, glow plug, fuel pump, fan) are on.
    *Igniting* means it has accepted a start but is not yet burning steadily
    (that takes about 4 to 5 minutes); *Running* means it has a flame and the
    glow plug is off. Below it is the thermostat's reason in plain words.
  - *Monitor only* (the mode after every restart): the thermostat decides
    but does not switch the heater. *Auto* (asks first): the thermostat
    switches the heater. Pressing Auto again also retries after 3 failed
    starts.
  - **Heater ON** (asks first) and **Heater OFF** switch the heater now. Any
    manual press puts the hub back in Monitor only, so the thermostat does
    not undo it. Heater ON is refused while the heater reports a fault, is
    not fully off, or the low-battery stop is holding it off; the reason
    shows in a note at the bottom of the screen.
  - *Heater details* opens the raw readings: link, state, parts, power,
    the heater's own set temperature, supply voltage, error code, firmware
    and last command.
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
  Bluetti took it.
- **Battery rules**:
  - *Heater low-battery stop*: a switch (turning it off asks first), the
    stop and restart levels, and what it is doing now.
  - *Fridge saver*: a switch, the Eco and Max levels, what it is doing now
    and its last change.

  Levels change in 5 % steps with **+** and **−** and go to the hub a
  moment after you stop tapping. Raising one level past the other moves the
  other too, to keep the 5 % gap the hub needs. Like the setpoint, they go
  back to the defaults when the hub restarts.
- **Footer**: the hub's clock and its address on another Wi-Fi, if joined.
  The page sets the hub's clock from the device's time when the hub has no
  internet time.

The Climate, Fridge and Power cards show `--` in place of readings while
their link is down, never the last values.

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
  Elite 100 V2, the same protocol family. They have not yet been tried on
  the Elite 200 V2 (see [Tested](#tested)).
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

- It is on after every restart. The levels, and an off switch, are on the
  dashboard's Battery rules card; a red alert shows at the top of the page
  while it holds the heater off.
- A heater that is already cooling down or off is left alone. The Off is
  repeated every 20 s while the heater still reports On or Starting.
- A lost Bluetti link neither trips it nor clears it. It needs a Bluetti
  reading under 30 s old to change state.
- It works in Monitor mode too. A start that was waiting to be sent when it
  trips is dropped.
- The stop level can be 5 to 50 %, and the resume level at least 5 points
  above it. The levels go back to the defaults when the hub restarts.

## Fridge saver

Switches the fridge to Eco when the Bluetti's charge falls to the low level,
and back to Max when it is up to the high level again (defaults 30 % and
50 %). The logic is in [`core/src/eco_guard.rs`](../core/src/eco_guard.rs).

- It is off after every restart. Turn it on and pick the levels on the
  dashboard's Battery rules card.
- It acts only when the charge crosses a level, and restores Max only if it
  was the one that set Eco. Change the mode by hand and it leaves it alone.
- It needs a Bluetti reading under 30 s old and a fridge status under 10 s
  old; without them it waits. A change the fridge does not take is retried
  every 30 s, three times at most.
- The low level can be 5 to 95 %, and the high level at least 5 points above
  it. The levels, and whether it is on, go back to the defaults when the hub
  restarts.

## Build and flash

Set up the toolchain first: [../docs/mac-setup.md](../docs/mac-setup.md).

If you have no `wifi.env`, skip the `source` line.

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

To work on the dashboard without the hub, serve `src/dashboard.html` from
any small web server that also answers `GET /api/state` with a saved copy of
the hub's JSON. The page only talks to the routes listed at the top of
[`src/web.rs`](src/web.rs).

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

Tested on the current hub (bare S3 module, PR #13):

- Booted, started the dashboard server, and received the sensor node's
  readings before the DHT11 was moved over.
- With the DHT11 on GPIO4: first reading 2 s after boot (22 °C, 21 % RH),
  then fresh readings every few seconds, and the thermostat acted on them.

Not yet tested:

- **The Bluetti's AC and DC switches.** The register addresses are borrowed
  from other Elite V2 models. If the station does not take a command, the
  dashboard's *Last command* says "not confirmed" and the switch moves back
  within 10 s.
- **The heater, fridge and Bluetti links on the current hub.** The devices
  were out of range when it was flashed. This also checks the shorter
  Bluetooth search time from PR #12, which has not yet been tried with the
  devices switched on.
- Handing over to the sensor node when the hub's DHT11 stops answering.
- **Stopping a burning heater with the low-battery stop.** The heater was
  off in the test above, so no Off was sent. The Off command itself is the
  one the dashboard's Heater OFF button sends, which is tested.
- Solar and charger input readings, and the sign of the charger reading.
  The Bluetti has only been read idle and with an AC load.
- The fridge saver and the heater stop at a real low charge. Both were
  triggered by moving their levels, not by draining the Bluetti.
- Joining another Wi-Fi (see [Networks](#networks)).
- The redesigned dashboard on the real tablet and phone, with live data.

## Thermostat defaults

| Setting | Value |
| --- | --- |
| Setpoint | 18 °C (change it on the dashboard) |
| Heat on | at or below setpoint − 1 °C |
| Heat off | at or above setpoint + 1 °C |
| Minimum on time | 10 minutes |
| Minimum off time | 10 minutes |
| Sensor timeout | 2 minutes without a reading turns heat off |

They live in `Config::default()` in `../core/src/thermostat.rs`. A setpoint
change from the dashboard lasts until the hub restarts.

## Low-battery defaults

| Setting | Value | After a restart |
| --- | --- | --- |
| Heater stop | at or below 15 % | on |
| Heater starts allowed again | at or above 25 % | |
| Fridge to Eco | at or below 30 % | off |
| Fridge back to Max | at or above 50 % | |

They live in `Config::default()` in `../core/src/heater_cutoff.rs` and
`../core/src/eco_guard.rs`.

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
  `M_PULSE_AP_PASS`.
- The hub only sends a command while it has a fresh status from the heater
  (under 10 seconds old).
- The hub restarts in Monitor mode. After a power cut it will not resume
  Auto by itself. The low-battery stop comes back on with its default
  levels, and the fridge saver comes back off.
