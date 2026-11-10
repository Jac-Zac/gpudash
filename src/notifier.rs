//! macOS notifications.
//!
//! Notifications sent through `osascript` always show the Script Editor icon. To show the
//! gpudash icon instead, gpudash carries a tiny helper app (built from notifier/main.swift)
//! and installs it at ~/Library/Application Support/gpudash/gpudash.app on first use.

use std::process::{Command, Stdio};

/// Post a notification in the background (the first one may wait on macOS's permission prompt).
pub fn notify(title: &str, body: &str, sound: bool) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || notify_blocking(&title, &body, sound));
}

/// Post a notification and wait until it's handed to macOS (for the background daemon).
pub fn notify_blocking(title: &str, body: &str, sound: bool) {
    #[cfg(notifier)]
    if bundle::post(title, body, sound) {
        return;
    }
    osascript(title, body, sound);
}

fn osascript(title: &str, body: &str, sound: bool) {
    // the text goes in as arguments, never into the script itself
    let mut script = "display notification (item 2 of argv) with title (item 1 of argv)".to_string();
    if sound {
        script += " sound name \"Glass\"";
    }
    // wait for it, so it doesn't linger as a zombie in the dashboard or the daemon
    let _ = Command::new("/usr/bin/osascript")
        .args(["-e", "on run argv", "-e", &script, "-e", "end run", "--", title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(notifier)]
mod bundle {
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    const BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/gpudash-notify"));
    const PLIST: &[u8] = include_bytes!("../notifier/Info.plist");
    const ICON: &[u8] = include_bytes!("../assets/gpudash.icns");

    fn version() -> String {
        let mut h = DefaultHasher::new();
        (BIN, PLIST, ICON).hash(&mut h);
        format!("{:x}", h.finish())
    }

    /// Install (or update) the helper app once per run. Returns its path.
    fn app() -> Option<&'static PathBuf> {
        static APP: OnceLock<Option<PathBuf>> = OnceLock::new();
        APP.get_or_init(|| install().ok()).as_ref()
    }

    fn install() -> std::io::Result<PathBuf> {
        let home = std::env::var_os("HOME").ok_or(std::io::ErrorKind::NotFound)?;
        let app = PathBuf::from(home).join("Library/Application Support/gpudash/gpudash.app");
        let stamp = app.with_file_name("gpudash.app.version");
        let version = version();
        let contents = app.join("Contents");
        let exe = contents.join("MacOS/gpudash-notify");
        if exe.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|v| v == version) {
            return Ok(app);
        }
        std::fs::create_dir_all(contents.join("MacOS"))?;
        std::fs::create_dir_all(contents.join("Resources"))?;
        write_new(&exe, BIN, 0o755)?;
        write_new(&contents.join("Info.plist"), PLIST, 0o644)?;
        write_new(&contents.join("Resources/gpudash.icns"), ICON, 0o644)?;
        // macOS wants app bundles signed; an ad-hoc signature is enough on your own machine
        let signed = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !signed.success() {
            return Err(std::io::Error::other("codesign failed"));
        }
        std::fs::write(&stamp, version)?;
        Ok(app)
    }

    /// Write a new file and rename it over `path`. Rewriting a signed binary in place (which
    /// another gpudash may be running) gets it killed by macOS; this swaps in a fresh file.
    /// The temp name is per process, so the dashboard and the daemon can't clobber each other.
    fn write_new(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// Run the helper directly, which hands it the text reliably. Until macOS knows the app
    /// (exit code 2: "not allowed"), launch it once through LaunchServices (`open`) instead;
    /// that registers it and shows the permission prompt.
    pub fn post(title: &str, body: &str, sound: bool) -> bool {
        let Some(app) = app() else { return false };
        let args = [title, body, if sound { "sound" } else { "" }];
        let direct = Command::new(app.join("Contents/MacOS/gpudash-notify"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match direct.map(|s| s.code()) {
            Ok(Some(0)) => true,
            Ok(Some(2)) => Command::new("/usr/bin/open")
                .args(["-g", "-n"])
                .arg(app)
                .arg("--args")
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success()),
            _ => false,
        }
    }
}
