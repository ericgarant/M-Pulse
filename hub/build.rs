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

    // The fallback password for the M-Pulse network is public in this
    // repository, so a release build must set its own. Debug builds keep the
    // fallback for the bench.
    let ap_pass = std::env::var("M_PULSE_AP_PASS").unwrap_or_default();
    if std::env::var("PROFILE").as_deref() == Ok("release") && ap_pass.is_empty() {
        eprintln!(
            "M_PULSE_AP_PASS is not set. A release build needs the M-Pulse network's own \
             password: put it in wifi.env and run `source ../wifi.env` first (see README.md, \
             Networks)."
        );
        std::process::exit(1);
    }
}
