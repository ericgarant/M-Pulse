# Toolchain setup on macOS

One-time setup for building and flashing M-Pulse firmware from a Mac. Run the
commands in Terminal.

## 1. Check what is already installed

```sh
rustup --version      # Rust installer
espup --version       # installs the ESP32 Rust toolchain
espflash --version    # flashes the board
ldproxy --version     # linker helper used by the ESP-IDF build
rustup toolchain list # should include a line that says "esp"
```

Anything that prints "command not found" (or a missing `esp` toolchain) needs
the matching step below. Skip the steps for tools you already have.

## 2. Install the prerequisites

```sh
# Apple's compiler tools
xcode-select --install

# Homebrew packages the ESP-IDF build needs (https://brew.sh if brew is missing)
brew install cmake ninja dfu-util python3

# Rust itself
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Open a new Terminal window after installing Rust so `cargo` is on your PATH.

## 3. Install the ESP32 Rust toolchain

The hub (ESP32-S3) and the optional backup sensor node (classic ESP32) both
use the Xtensa architecture, which needs Espressif's build of the Rust
compiler. `espup` installs it. If you only build the hub, `--targets esp32s3`
is enough.

```sh
cargo install espup espflash ldproxy --locked
espup install --targets esp32,esp32s3
```

`espup` writes a file called `~/export-esp.sh`. It must be loaded in every
Terminal session before building. To make that automatic:

```sh
echo '. $HOME/export-esp.sh' >> ~/.zshrc
source ~/.zshrc
```

## 4. Build and flash

Plug the board in with a USB-C data cable, then:

```sh
cd hub            # or sensor-node, only if you use the backup sensor
cargo run --release
```

The first build downloads and compiles ESP-IDF, which takes 10 to 20 minutes
and about 5 GB of disk. Later builds take well under a minute.

`cargo run` flashes the board and opens the serial monitor. Press `Ctrl+C` to
leave the monitor. Opening the monitor restarts the board, which puts the hub
back in Monitor mode with its default low-battery levels.

To check the shared logic without a board, run `cargo test` in `core/`.

The hub README explains the optional `wifi.env` file with the network
password; build with it loaded (`source ../wifi.env`) when you have one.

## Troubleshooting

- **No serial port found.** The hub shows up as `USB Single Serial`, a
  `/dev/cu.usbmodem...` port (see the hub README for the one on this Mac).
  That name comes from a WCH CH343 USB serial chip; if the port never
  appears, install WCH's CH34x driver (wch-ic.com). The sensor node's
  Freenove ESP32 WROOM board uses a CH340, which recent macOS versions
  support. If nothing shows up under `ls /dev/cu.*`, try another cable
  first, because many USB-C cables are charge-only.
- **"Failed to connect" while flashing.** Hold the `BOOT` button on the board
  while espflash prints "Connecting...", then release it.
- **`error: toolchain 'esp' is not installed`.** Run `espup install --targets
  esp32,esp32s3` again and make sure `~/export-esp.sh` is loaded.
- **Build fails with a long path error.** Keep the repo in a short path such
  as `~/code/M-Pulse`. ESP-IDF does not like spaces in the path.
