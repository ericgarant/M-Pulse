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

The classic ESP32 uses the Xtensa architecture, which needs Espressif's build
of the Rust compiler. `espup` installs it.

```sh
cargo install espup espflash ldproxy --locked
espup install --targets esp32
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
cd thermostat
cargo run --release
```

The first build downloads and compiles ESP-IDF, which takes 10 to 20 minutes
and about 5 GB of disk. Later builds take well under a minute.

`cargo run` flashes the board and opens the serial monitor. Press `Ctrl+C` to
leave the monitor.

## Troubleshooting

- **No serial port found.** The Freenove ESP32 WROOM board uses a CH340 USB
  serial chip. Recent macOS versions include the driver. If nothing shows up
  under `ls /dev/cu.*`, try another cable first, because many USB-C cables are
  charge-only.
- **"Failed to connect" while flashing.** Hold the `BOOT` button on the board
  while espflash prints "Connecting...", then release it.
- **`error: toolchain 'esp' is not installed`.** Run `espup install --targets
  esp32` again and make sure `~/export-esp.sh` is loaded.
- **Build fails with a long path error.** Keep the repo in a short path such
  as `~/code/M-Pulse`. ESP-IDF does not like spaces in the path.
