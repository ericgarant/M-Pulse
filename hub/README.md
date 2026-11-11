# Hub (ESP32-S3)

Firmware for the Freenove ESP32-S3 media kit board. It:

- keeps a BLE link to the BougeRV diesel heater (`BougeRV-DHG001`) and
  reconnects if it drops
- keeps a second BLE link to the BougeRV Rocky 50 fridge (`SYZ-R50-...`),
  reads it every 2 s, and lets the dashboard change it (see
  [Fridge](#fridge))
- keeps a third BLE link to the Bluetti Elite 200 V2 power station and reads
  its battery, inputs and outputs every 5 s, read only (see
  [Bluetti](#bluetti))
- can put the fridge in Eco while the Bluetti is low and back to Max once it
  recovers (the fridge saver, off after every restart)
- receives cabin readings from the [sensor node](../sensor-node) over Wi-Fi
- runs the thermostat on those readings
- switches the heater on and off, either from the dashboard's On and Off
  buttons or by the thermostat in Auto mode. After every restart the hub is
  in Monitor mode and sends no switch commands until you choose otherwise
- serves a dashboard at http://192.168.71.1 on the hub network
- sets the heater's clock from internet time (SNTP), or from your phone's
  time when you open the dashboard without internet

The heater link lives here because the classic ESP32 could not hold it: the
heater went silent about 11 s into every connection. The S3 and a Mac both
hold the same link indefinitely.

## Networks

The hub runs its own Wi-Fi network, `M-Pulse`. The sensor node and your phone
join it, so everything works in the camper with no router. This is the tested
setup.

To set your own password for that network, put it in `wifi.env` at the top of
the repository. Git ignores this file, so the password stays on your computer:

```sh
export M_PULSE_AP_PASS="password for the M-Pulse network"   # 8+ characters
```

Without it the password is `mpulse-heater`. Flash both boards from the same
`wifi.env`, or the sensor node cannot join.

### Joining another Wi-Fi (optional, untested)

The firmware can also join an existing network, such as your home Wi-Fi, for
internet time and to show the dashboard there. It is built in but has not been
tested, and it has trade-offs:

- While the hub is on another network, its own network moves to that
  network's channel, so phones and the sensor node may drop off briefly when
  it joins.
- If the sensor node joins that network too and the hub is not on it, the
  readings stop reaching the hub. Auto then treats the sensor as lost.

To try it, add these lines to `wifi.env` and rebuild both boards:

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

- **Cabin** shows the latest reading from the sensor node and its age.
- **Thermostat** changes the setpoint in 0.5 °C steps and chooses the mode:
  - *Monitor only* (the mode after every restart): the thermostat decides and
    shows its reason, but does not switch the heater.
  - *Auto*: the thermostat switches the heater. Pressing Auto again also
    retries after 3 failed starts.
- **Manual** switches the heater on or off now. Any manual button press puts
  the hub back in Monitor only, so the thermostat does not undo it. Heater ON
  is refused while the heater reports a fault or is not fully off.
- **Heater** shows the link and what the heater reports. *Igniting* means it
  has accepted a start but is not yet burning steadily (that takes about
  4 to 5 minutes); *Running* means it has a flame and the glow plug is off.
  The details show `--` while the link is down.

- **Fridge** shows the temperature inside, the target, the mode, the input
  voltage, the battery protection level and the panel lock. **+** and **−**
  change the target by 1 degree; **Eco** and **Max** change the mode;
  **Fridge ON** and **Fridge OFF** switch it (Off asks first). A red note
  appears when the fridge has stayed more than 4 °C above its target for
  30 minutes: check the lid and the power.

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
- The heater and fridge links take turns to search and connect, because
  NimBLE allows one scan or connection attempt at a time.
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
  every 5 s: charge %, solar and charger input, AC and DC output, and
  whether the AC and DC outputs are on. It writes nothing else.
- While the hub holds the link, the Bluetti app can only reach the station
  over the cloud, not Bluetooth.

## Heater low-battery stop

When the Bluetti's charge falls to the stop level (default 15 %), the hub
sends the heater its normal Off, so it runs its full cool-down and the glow
plug is cleared properly, and refuses to start it (thermostat and the
dashboard's On button). Starts are allowed again once the charge is back to
the resume level (default 25 %); in Auto the thermostat may then restart the
heater, in Monitor it stays off until switched on. The logic is in
[`core/src/heater_cutoff.rs`](../core/src/heater_cutoff.rs).

- It is on after every restart. The levels, and an off switch, are on the
  dashboard's Power card; a red notice shows on the Thermostat card while it
  holds the heater off.
- A heater that is already cooling down or off is left alone. The Off is
  repeated every 20 s while the heater still reports On or Starting.
- A lost Bluetti link neither trips it nor clears it.

## Fridge saver

Switches the fridge to Eco when the Bluetti's charge falls to the low level,
and back to Max when it is up to the high level again (defaults 30 % and
50 %). The logic is in [`core/src/eco_guard.rs`](../core/src/eco_guard.rs).

- It is off after every restart. Turn it on and pick the levels on the
  dashboard's Power card.
- It acts only when the charge crosses a level, and restores Max only if it
  was the one that set Eco. Change the mode by hand and it leaves it alone.
- It needs a Bluetti reading under 30 s old and a fridge status under 10 s
  old; without them it waits. A change the fridge does not take is retried
  every 30 s, three times at most.

## Build and flash

Set up the toolchain first: [../docs/mac-setup.md](../docs/mac-setup.md).

If you have no `wifi.env`, skip the `source` line.

```sh
source ../wifi.env
cargo build --release
espflash flash --monitor --partition-table partitions.csv target/xtensa-esp32s3-espidf/release/m-pulse-hub
```

## Tested

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

- The key exchange finishes about 1.5 s after connecting, and the station
  answers every read within 0.1 s. All three links held together.
- First read: model `Elite200V2`, 94 %, AC and DC outputs on, no load and no
  input. The station reports 5994 min to full and to empty when it has no
  estimate, so the dashboard hides that.
- Battery voltage is in hundredths (the app's divide-by-10 is wrong for this
  model): 39.66 V at 2.2 A out matched the 83 W AC load. Charging-status
  code 2 means discharging.
- Fridge saver: with the levels set to 95/100 % (charge was 91 %) the fridge
  went to Eco within seconds; moving them to 5/10 % put it back to Max.
- Heater stop: with the stop level above the charge, it tripped within a
  second and cleared when the levels were lowered. The heater was off, so
  no Off was needed; stopping a burning heater is not yet tested live.

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

## Safety notes

- The heater has no idea the hub exists. If the hub loses power or its
  Bluetooth link while the heater is running, **the heater keeps running** at
  its own setting until you switch it off at the heater or its remote.
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
  Auto by itself.
