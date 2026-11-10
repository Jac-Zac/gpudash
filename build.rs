//! On macOS, compile the small Swift helper that posts notifications with the gpudash icon.
//! If Swift isn't available, gpudash falls back to `osascript` (Script Editor icon).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(notifier)");
    println!("cargo::rerun-if-changed=notifier/main.swift");
    println!("cargo::rerun-if-changed=notifier/Info.plist");
    println!("cargo::rerun-if-changed=assets/gpudash.icns");

    // the commit this was built from, for `gpudash --version`
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown commit".into());
    println!("cargo::rustc-env=GPUDASH_COMMIT={commit}");
    // only watch git files that exist: a missing path makes cargo rerun this script (and
    // recompile the Swift helper) on every build, e.g. when built from a crates.io tarball
    let git_paths = Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD", "--git-path", "refs/heads"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    for path in git_paths.lines().filter(|p| std::path::Path::new(p).exists()) {
        println!("cargo::rerun-if-changed={path}");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("gpudash-notify");
    // build for the target's architecture (not the host's), at the Info.plist's minimum macOS
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64".to_string(),
        Ok(a) => a.to_string(),
        Err(_) => String::new(),
    };
    let target = format!("{arch}-apple-macos11");
    // `swiftc` comes with Xcode or the Command Line Tools (xcode-select --install);
    // `xcrun` finds it even when it isn't on PATH.
    let compile = |program: &str, pre: &[&str]| {
        Command::new(program)
            .args(pre)
            .args(["-target", &target])
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
