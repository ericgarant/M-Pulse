fn main() {
    // Passes the ESP-IDF build settings (cfg flags, link args) on to this crate.
    embuild::espidf::sysenv::output();

    // Wi-Fi and Bluetti settings are read at build time; rebuild when they
    // change.
    for var in [
        "M_PULSE_WIFI_SSID",
        "M_PULSE_WIFI_PASS",
        "M_PULSE_AP_PASS",
        "M_PULSE_BLUETTI_KEEP_ON",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
