fn main() {
    // Passes the ESP-IDF build settings (cfg flags, link args) on to this crate.
    embuild::espidf::sysenv::output();

    // Wi-Fi and name settings are read at build time; rebuild when they change.
    for var in [
        "M_PULSE_WIFI_SSID",
        "M_PULSE_WIFI_PASS",
        "M_PULSE_AP_PASS",
        "M_PULSE_CAMERA_NAME",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    // The fallback password for the hub's network is public in this
    // repository, so a release build must use the hub's own. Debug builds keep
    // the fallback for the bench.
    let ap_pass = std::env::var("M_PULSE_AP_PASS").unwrap_or_default();
    if std::env::var("PROFILE").as_deref() == Ok("release") && ap_pass.is_empty() {
        eprintln!(
            "M_PULSE_AP_PASS is not set. A release build needs the hub network's own \
             password: run `source ../wifi.env` first (see ../hub/README.md, Networks)."
        );
        std::process::exit(1);
    }
}
