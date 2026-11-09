fn main() {
    // Passes the ESP-IDF build settings (cfg flags, link args) on to this crate.
    embuild::espidf::sysenv::output();
}
