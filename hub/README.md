# Hub (ESP32-S3)

Firmware for the Freenove ESP32-S3 media kit board. It:

- keeps a BLE link to the BougeRV diesel heater (`BougeRV-DHG001`) and
  reconnects if it drops
- keeps a second BLE link to the BougeRV Rocky 50 fridge (`SYZ-R50-...`),
  reads it every 2 s, and lets the dashboard change it (see
  [Fridge](#fridge))
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
