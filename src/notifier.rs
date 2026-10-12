//! macOS notifications.
//!
//! Notifications sent through `osascript` always show the Script Editor icon. To show the
//! gpudash icon instead, gpudash carries a tiny helper app (built from notifier/main.swift)
//! and installs it at ~/Library/Application Support/gpudash/gpudash.app on first use.

use std::process::{Command, Stdio};

/// Post a notification in the background (the first one may wait on macOS's permission prompt).
pub fn notify(title: &str, body: &str, sound: bool) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        #[cfg(notifier)]
        if bundle::post(&title, &body, sound) {
            return;
        }
        osascript(&title, &body, sound);
    });
}

fn osascript(title: &str, body: &str, sound: bool) {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
    if sound {
        script += " sound name \"Glass\"";
    }
    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(notifier)]
mod bundle {
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
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
        if std::fs::read_to_string(&stamp).is_ok_and(|v| v == version) {
            return Ok(app);
        }
        let contents = app.join("Contents");
        std::fs::create_dir_all(contents.join("MacOS"))?;
        std::fs::create_dir_all(contents.join("Resources"))?;
        let exe = contents.join("MacOS/gpudash-notify");
        std::fs::write(&exe, BIN)?;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
        std::fs::write(contents.join("Info.plist"), PLIST)?;
        std::fs::write(contents.join("Resources/gpudash.icns"), ICON)?;
        // macOS wants app bundles signed; an ad-hoc signature is enough on your own machine
        let signed = Command::new("codesign")
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
            Ok(Some(2)) => Command::new("open")
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
