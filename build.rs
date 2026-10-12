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
    // `swiftc` comes with Xcode or the Command Line Tools (xcode-select --install);
    // `xcrun` finds it even when it isn't on PATH.
    let compile = |program: &str, pre: &[&str]| {
        Command::new(program)
            .args(pre)
            .args(["-O", "-suppress-warnings", "notifier/main.swift", "-o"])
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success())
    };
    if compile("swiftc", &[]) || compile("xcrun", &["swiftc"]) {
        println!("cargo::rustc-cfg=notifier");
    } else {
        println!(
            "cargo::warning=Swift compiler not found, so notifications will show the Script Editor icon. \
             Run `xcode-select --install`, then reinstall gpudash."
        );
    }
}
