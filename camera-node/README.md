# Camera node (ESP32-WROVER)

Firmware for the Freenove ESP32-WROVER camera board with an OV3660. The
camera sits in **standby** until the hub asks for video. Then it streams
MJPEG on the hub's `M-Pulse` network and goes back to standby when the hub
stops it. As a backup, it also goes to standby when nobody has watched for
5 minutes.

The hub's dashboard shows each camera on its Cameras page. The protocol is
in [core/src/camera.rs](../core/src/camera.rs).

## What it serves

Two HTTP servers on the camera's address (the hub's network hands it an
address). The stream has its own server on port 81, because an ESP-IDF HTTP
server handles one request at a time: a running stream would otherwise hold
up the photo and settings routes.

| Port | Path | When | Returns |
| --- | --- | --- | --- |
| 81 | `/stream` | while streaming | MJPEG, one viewer at a time (a second viewer waits) |
| 80 | `/` | always | a small page with the live view |
| 80 | `/still.jpg` | while streaming | one JPEG frame, with `Access-Control-Allow-Origin: *` so the hub's page can fetch it from another origin |
| 80 | `/settings` | always | the active frame size and JPEG quality, as plain text |
| 80 | `/settings` (POST) | while streaming | changes the frame size and quality (see below) |

In standby, the video routes answer `503`. A `503` from `/still.jpg` also
carries the CORS header.

### Settings

`GET /settings` returns the active values:

```
size=SVGA
quality=12
```

`POST /settings` takes a form body (`Content-Type` is not checked, so
`application/x-www-form-urlencoded` is the usual choice) with two fields:

- `size`: one of `VGA`, `SVGA`, `XGA`, `SXGA`.
- `quality`: a whole number from 10 to 40. Lower is better quality and a
  bigger JPEG.

Example: `curl -d "size=SVGA&quality=14" http://<camera>/settings`

It replies `200` with the new values in the same format as the GET. It
replies `400` for bad input, `409` if the camera is not streaming (the
values can only be applied to a running sensor), and `500` if the sensor
rejects the change. A `500` leaves the old values in place. The values are
kept in memory, so the next start uses them. They reset to `SVGA` and
quality `12` on reboot. A new frame size restarts the camera driver for a
moment, because the driver sizes its frame buffers when it starts.

On the bench, one camera at a time, SVGA at quality 12 ran at about 5 to 12
frames a second on a good board (85 to 290 KB/s). Boards differ a lot: one
board managed only 1 to 2 frames a second with a strong signal and was
retired. Quality 10 made frames only about 10% bigger and slower, with no
visible gain. To compare boards, run each alone on the same scene and read
the stats line below.

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

## Power and Wi-Fi

This board has no power-down pin, so standby stops the camera clock and the
driver, but the sensor stays on the 3.3 V rail. Wi-Fi power save is on in
standby and off while streaming, where it cut the frame rate to about a
third. Each connection has a 32 KB TCP send buffer (the ESP-IDF default is
about 5.7 KB). The standby current has not
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
   - `HTTP up: port 80 (page, still, settings), port 81 (stream)`
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
   off screen so nothing is viewing the stream. After 5 minutes the camera
   log shows `No viewers for 300 s: camera to standby`. This is the backup;
   the hub's Stop is the normal way to sleep. The hub does not start it
   again; press Stop and then Watch to wake it.
7. Settings: with the camera streaming, `curl -d "size=SVGA&quality=14"
   http://<camera>/settings` should reply `size=SVGA` and `quality=14`. The
   camera log shows `Settings: SVGA quality 14`. Check the picture is
   larger. Then `curl http://<camera>/settings` should show the same values.

Other lines to check in the logs:

- `stream started` and `stream stopped`: a viewer connected and left.
- `Stream SVGA q12: 9.7 fps, 16 KB per frame, 163 KB/s, RSSI -36 dBm`:
  every 10 s while streaming, the frame rate, average frame size,
  throughput and Wi-Fi signal strength (closer to 0 is stronger).
- `Could not start the camera: ...`: the sensor did not start. The camera
  returns to standby and the hub reports no answer after 15 tries.
- `Ignoring a command from ...`: something other than the hub sent a
  message to port 4221.
- `Announce failed: ...`: the broadcast to the hub did not send.
