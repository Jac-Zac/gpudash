//! On macOS, compile the small Swift helper that posts notifications with the gpudash icon.
//! If Swift isn't available, gpudash falls back to `osascript` (Script Editor icon).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(notifier)");
    println!("cargo::rerun-if-changed=notifier/main.swift");
    println!("cargo::rerun-if-changed=notifier/Info.plist");
    println!("cargo::rerun-if-changed=assets/gpudash.icns");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("gpudash-notify");
    let built = Command::new("swiftc")
        .args(["-O", "-suppress-warnings", "notifier/main.swift", "-o"])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    if built {
        println!("cargo::rustc-cfg=notifier");
    } else {
        println!("cargo::warning=swiftc not found: notifications will show the Script Editor icon");
    }
}
