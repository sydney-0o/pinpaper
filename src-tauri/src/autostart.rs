//! Small, dependency-free integration with the three desktop autostart
//! mechanisms supported by Pinpaper's release builds.
//!
//! Keeping this here instead of asking users to create files themselves means
//! the setting works for a portable build as well as an installed bundle. The
//! setting is opt-in and can be removed from the same UI at any time.

#[cfg(target_os = "windows")]
use std::process::Command;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

const APP_ID: &str = "app.pinpaper.desktop";

fn home_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let variable = "USERPROFILE";
    #[cfg(not(target_os = "windows"))]
    let variable = "HOME";
    env::var_os(variable)
        .map(PathBuf::from)
        .ok_or_else(|| format!("Cannot find the {variable} folder"))
}

fn executable() -> Result<PathBuf, String> {
    // AppImage runs its embedded binary from a temporary mount. The runtime
    // exposes the real, movable AppImage path in APPIMAGE; using current_exe
    // there would leave a startup entry pointing at a path that disappears
    // when the app closes.
    #[cfg(target_os = "linux")]
    if let Some(appimage) = env::var_os("APPIMAGE") {
        return Ok(PathBuf::from(appimage));
    }
    env::current_exe().map_err(|_| "Cannot find the Pinpaper executable".into())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn desktop_exec_escape(path: &str) -> String {
    // The desktop-entry Exec grammar treats a quoted path as one argument.
    // Backslashes and quotes are escaped before the surrounding quotes.
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
fn registration_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{APP_ID}.plist"))
}

#[cfg(target_os = "linux")]
fn registration_path(home: &Path) -> PathBuf {
    // APP_ID already ends in `.desktop`, which is the required filename
    // suffix for XDG autostart entries.
    home.join(".config/autostart").join(APP_ID)
}

#[cfg(target_os = "windows")]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(target_os = "macos")]
fn enable_unix(home: &Path, exe: &Path) -> Result<(), String> {
    let target = registration_path(home);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|_| "Cannot create the macOS login-items folder")?;
    }
    let program = xml_escape(&exe.to_string_lossy());
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>{APP_ID}</string>\n\
         <key>ProgramArguments</key><array><string>{program}</string></array>\n\
         <key>RunAtLoad</key><true/>\n\
         <key>ProcessType</key><string>Interactive</string>\n\
         </dict></plist>\n"
    );
    let temp = target.with_extension("plist.tmp");
    fs::write(&temp, body).map_err(|_| "Cannot write the macOS login item")?;
    fs::rename(temp, target).map_err(|_| "Cannot register Pinpaper at macOS login".into())
}

#[cfg(target_os = "macos")]
fn disable_unix(home: &Path) -> Result<(), String> {
    match fs::remove_file(registration_path(home)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Cannot remove Pinpaper from macOS login items".into()),
    }
}

#[cfg(target_os = "linux")]
fn enable_unix(home: &Path, exe: &Path) -> Result<(), String> {
    let target = registration_path(home);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|_| "Cannot create the Linux autostart folder")?;
    }
    let command = desktop_exec_escape(&exe.to_string_lossy());
    let body = format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName=Pinpaper\nComment=Change your wallpaper from your saved pictures\nExec={command}\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n"
    );
    let temp = target.with_extension("desktop.tmp");
    fs::write(&temp, body).map_err(|_| "Cannot write the Linux autostart entry")?;
    fs::rename(temp, target).map_err(|_| "Cannot register Pinpaper at Linux login".into())
}

#[cfg(target_os = "linux")]
fn disable_unix(home: &Path) -> Result<(), String> {
    match fs::remove_file(registration_path(home)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Cannot remove Pinpaper from Linux autostart".into()),
    }
}

#[cfg(target_os = "windows")]
fn enable_windows(exe: &Path) -> Result<(), String> {
    let value = format!("\"{}\"", exe.to_string_lossy());
    let status = Command::new("reg")
        .args([
            "add",
            RUN_KEY,
            "/v",
            "Pinpaper",
            "/t",
            "REG_SZ",
            "/d",
            value.as_str(),
            "/f",
        ])
        .status()
        .map_err(|_| "Windows Registry is unavailable")?;
    if status.success() {
        Ok(())
    } else {
        Err("Cannot register Pinpaper at Windows login".into())
    }
}

#[cfg(target_os = "windows")]
fn disable_windows() -> Result<(), String> {
    let status = Command::new("reg")
        .args(["delete", RUN_KEY, "/v", "Pinpaper", "/f"])
        .status()
        .map_err(|_| "Windows Registry is unavailable")?;
    // `reg delete` returns 1 when the value does not exist. Removal is
    // intentionally idempotent, so that is already the desired state.
    if status.success() || status.code() == Some(1) {
        Ok(())
    } else {
        Err("Cannot remove Pinpaper from Windows startup".into())
    }
}

pub fn set(enabled: bool) -> Result<(), String> {
    let exe = executable()?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let home = home_dir()?;
        if enabled {
            enable_unix(&home, &exe)
        } else {
            disable_unix(&home)
        }
    }
    #[cfg(target_os = "windows")]
    {
        if enabled {
            enable_windows(&exe)
        } else {
            disable_windows()
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (enabled, exe);
        Err("Autostart is not supported on this platform".into())
    }
}

pub fn is_enabled() -> Result<bool, String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        return Ok(registration_path(&home_dir()?).exists());
    }
    #[cfg(target_os = "windows")]
    {
        let status = Command::new("reg")
            .args(["query", RUN_KEY, "/v", "Pinpaper"])
            .status()
            .map_err(|_| "Windows Registry is unavailable")?;
        return Ok(status.success());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    Err("Autostart is not supported on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::{desktop_exec_escape, xml_escape};

    #[test]
    fn escapes_launch_agent_xml() {
        assert_eq!(xml_escape("a&<b>\"'"), "a&amp;&lt;b&gt;&quot;&apos;");
    }

    #[test]
    fn quotes_desktop_entry_paths() {
        assert_eq!(
            desktop_exec_escape(r#"/tmp/a b\pinpaper"#),
            r#""/tmp/a b\\pinpaper""#
        );
    }
}
