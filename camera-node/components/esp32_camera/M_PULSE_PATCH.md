# Local changes to espressif/esp32-camera 2.0.7

This is the Espressif ESP32 camera driver, version 2.0.7 from the component
registry, licensed under Apache-2.0 (see `LICENSE`). Its `examples/`, `test/`
and checksum files are left out. The one change is in `target/xclk.c`:

- `ch_conf` is now zero-initialised and `sleep_mode` is set to
  `LEDC_SLEEP_MODE_NO_ALIVE_NO_PD`.

Reason: ESP-IDF 5.5 validates `sleep_mode` in `ledc_channel_config()`. Version
2.0.7 leaves the field unset, so the camera clock fails to start with
`sleep_mode argument is invalid`. Remove this patch when a registry release
sets the field itself.
