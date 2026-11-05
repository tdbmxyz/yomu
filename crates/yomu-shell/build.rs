fn main() {
    // Build scripts run on the host, so inspect the target rather than cfg!.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        tauri_build::build();
    }
}
