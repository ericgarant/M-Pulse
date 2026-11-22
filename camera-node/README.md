# Camera node (ESP32-WROVER)

Firmware for the Freenove ESP32-WROVER camera board with an OV3660. The
camera sits in **standby** until the hub asks for video. Then it streams
MJPEG on the hub's `M-Pulse` network and goes back to standby when the hub
stops it, or when nobody has watched for 30 seconds.

The hub's dashboard shows each camera on its Cameras page. The protocol is
in [core/src/camera.rs](../core/src/camera.rs).

## What it serves

Port 80 on the camera's address (the hub's network hands it an address):

| Path | When | Returns |
| --- | --- | --- |
| `/` | always | a small page with the live view |
| `/stream` | while streaming | MJPEG, at most 2 viewers at once |
| `/still.jpg` | while streaming | one JPEG frame |

In standby, the two video routes answer `503`.

## What it sends and receives

- Every 5 s, and at once on a state change, it broadcasts
  `MPC1 announce <name> <state>` to UDP port 4220 on the hub.
  The state is `standby`, `starting` or `streaming`.
- It listens on UDP port 4221 for `MPC1 start` and `MPC1 stop` from the hub.

## Build settings

Set these the same way as the sensor node (see
[../hub/README.md](../hub/README.md)):

- `M_PULSE_CAMERA_NAME`: the name shown on the dashboard. Use a different
  name on each camera, for example `camper` or `cab`. Lower-case letters,
  digits and `-` only, up to 16 characters. It defaults to `camper`.
- `M_PULSE_WIFI_SSID`, `M_PULSE_WIFI_PASS`, `M_PULSE_AP_PASS`: as for the
  sensor node. If `M_PULSE_WIFI_SSID` is set, the camera tries that network
  first and then the hub's `M-Pulse` network, in turns, until one answers.

## Build and flash

```sh
source ../wifi.env
M_PULSE_CAMERA_NAME=cab cargo build --release
espflash flash --monitor --partition-table partitions.csv target/xtensa-esp32-espidf/release/m-pulse-camera-node
```

The camera driver is vendored in `components/esp32_camera` with one local
change, recorded in
[components/esp32_camera/M_PULSE_PATCH.md](components/esp32_camera/M_PULSE_PATCH.md).

## Power

This board has no power-down pin, so standby stops the camera clock and the
driver, but the sensor stays on the 3.3 V rail. The standby current has not
been measured yet. See [../hub/README.md](../hub/README.md#cameras).

## Hardware notes

- Camera pins are the Freenove WROVER layout: XCLK 21, SCCB 26 and 27, the
  data lines on 4, 5, 18, 19, 36, 39, 34 and 35, VSYNC 25, HREF 23, PCLK 22.
- The camera must be seated on its ribbon connector with the latch closed.
- Power from the board's 5 V pin, not only from USB.

## Testing

Bench steps used so far. The camera needs the hub running, or a phone
joined to the hub's `M-Pulse` network with the dashboard open.

1. Build and flash with the camera's name (see Build settings). The
   monitor opens after the flash.
2. Check the boot log, in this order:
   - `M-Pulse camera node "<name>" starting, camera in standby`
   - `Joined "M-Pulse" as <address>` (or your home network's name, if
     `M_PULSE_WIFI_SSID` is set and it joins that first)
   - `HTTP server up on port 80`
3. On the hub's network, open the dashboard and go to the Cameras page. The
   tile for the camera should appear within a few seconds. The hub's log
   should show `Camera "<name>" announced from <address>`.
4. Opening the page starts the camera. The camera log should show
   `Start requested: powering up the camera`, then
   `Camera sensor PID 0x3660` (or `0x0026` for the OV2640 board), then
   `Camera streaming`. The tile should show the picture.
5. Press Rotate once to check the picture turns, then leave the page. The
   camera log should show `Camera in standby` within a few seconds.
6. Standby on its own: with the page still open, scroll the camera's tile
   off screen so nothing is viewing the stream. After 30 s the camera log
   shows `No viewers for 30 s: camera to standby`. The hub does not start it
   again; press Stop and then Watch to wake it.

Other lines to check in the logs:

- `stream started` and `stream stopped`: a viewer connected and left.
- `Could not start the camera: ...`: the sensor did not start. The camera
  returns to standby and the hub reports no answer after 15 tries.
- `Ignoring a command from ...`: something other than the hub sent a
  message to port 4221.
- `Announce failed: ...`: the broadcast to the hub did not send.
