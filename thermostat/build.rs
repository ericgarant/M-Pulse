fn main() {
    // Passes the ESP-IDF build settings (cfg flags, link args) on to this crate.
    embuild::espidf::sysenv::output();

    // Local time of the build machine for the heater clock sync, set as
    // `M_PULSE_CLOCK=$(date +%y%m%d%H%M%S)`. Rebuild whenever it changes.
    println!("cargo:rerun-if-env-changed=M_PULSE_CLOCK");
}
