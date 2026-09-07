use crate::{model::Pin, network};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
const MAX_DOWNLOAD: u64 = 25 * 1024 * 1024;
const CACHE_LIMIT: u64 = 256 * 1024 * 1024;
pub fn cached(dir: &Path, pin: &Pin) -> PathBuf {
    dir.join(format!("{:x}.jpg", Sha256::digest(pin.url.as_bytes())))
}
pub fn download(dir: &Path, pin: &Pin) -> Result<PathBuf, String> {
    if !network::valid_image_url(&pin.url) {
        return Err("Image is not hosted on Pinterest's image CDN".into());
    }
    fs::create_dir_all(dir).map_err(|_| "Cannot create image cache")?;
    let path = cached(dir, pin);
    if path.exists() {
        return Ok(path);
    }
    let response = network::client()?
        .get(&pin.url)
        .send()
        .map_err(|_| "Image download failed")?;
    if !response.status().is_success() {
        return Err(format!(
            "Image download returned HTTP {}",
            response.status()
        ));
    }
    if response.content_length().unwrap_or(0) > MAX_DOWNLOAD {
        return Err("Image exceeds 25 MB".into());
    }
    let mut bytes = vec![];
    response
        .take(MAX_DOWNLOAD + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Image download interrupted")?;
    if bytes.len() as u64 > MAX_DOWNLOAD {
        return Err("Image exceeds 25 MB".into());
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Unknown image format")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| "Image cannot be decoded within safety limits")?;
    if decoded.width() < pin.width.min(512) {
        return Err("Image resolution does not match metadata".into());
    }
    let temp = path.with_extension("tmp");
    image::DynamicImage::ImageRgb8(decoded.to_rgb8())
        .save_with_format(&temp, image::ImageFormat::Jpeg)
        .map_err(|_| "Cannot write image cache")?;
    fs::rename(&temp, &path).map_err(|_| "Cannot finish cached image")?;
    Ok(path)
}
pub fn prune(dir: &Path, keep: &[PathBuf]) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()?.to_str()? != "jpg" {
                return None;
            }
            let m = e.metadata().ok()?;
            Some((p, m.len(), m.modified().ok()?))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.2);
    for (path, size, _) in files {
        if total <= CACHE_LIMIT {
            break;
        }
        if !keep.contains(&path) && fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}
#[cfg(target_os = "macos")]
pub fn apply(app: &tauri::AppHandle, path: &Path) -> Result<(), String> {
    let path = path.to_path_buf();
    let perform = move || -> Result<(), String> {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSScreen, NSWorkspace};
        use objc2_foundation::{NSDictionary, NSString, NSURL};
        let main =
            MainThreadMarker::new().ok_or("Wallpaper update requires the macOS main thread")?;
        let path = path.to_str().ok_or("Invalid wallpaper file path")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let workspace = NSWorkspace::sharedWorkspace();
        let screens = NSScreen::screens(main);
        if screens.is_empty() {
            return Err("No connected display found".into());
        }
        for screen in screens.iter() {
            let options = workspace
                .desktopImageOptionsForScreen(&screen)
                .unwrap_or_else(NSDictionary::new);
            // Options originate from NSWorkspace; AppKit calls run on the main thread.
            unsafe {
                workspace.setDesktopImageURL_forScreen_options_error(&url, &screen, &options)
            }
            .map_err(|error| {
                format!(
                    "macOS could not set the wallpaper: {}",
                    error.localizedDescription()
                )
            })?;
        }
        Ok(())
    };
    if objc2::MainThreadMarker::new().is_some() {
        return perform();
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(perform());
    })
    .map_err(|_| "Could not schedule macOS wallpaper update")?;
    receiver
        .recv()
        .map_err(|_| "macOS wallpaper update interrupted")?
}
#[cfg(target_os = "windows")]
pub fn apply(_app: &tauri::AppHandle, path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_SETDESKWALLPAPER,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            wide.as_mut_ptr().cast(),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        Err("Windows could not set the desktop wallpaper".into())
    }
}
#[cfg(target_os = "linux")]
pub fn apply(_app: &tauri::AppHandle, path: &Path) -> Result<(), String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    let uri = url::Url::from_file_path(path)
        .map_err(|_| "Invalid wallpaper path")?
        .to_string();
    let run = |schema: &str, key: &str, value: &str| -> Result<(), String> {
        let ok = std::process::Command::new("gsettings")
            .args(["set", schema, key, value])
            .status()
            .map_err(|_| "gsettings is unavailable")?;
        if ok.success() {
            Ok(())
        } else {
            Err("Desktop rejected wallpaper setting".into())
        }
    };
    if desktop.contains("gnome") || desktop.contains("unity") || desktop.contains("budgie") {
        run("org.gnome.desktop.background", "picture-uri", &uri)?;
        run("org.gnome.desktop.background", "picture-uri-dark", &uri)
    } else if desktop.contains("cinnamon") {
        run("org.cinnamon.desktop.background", "picture-uri", &uri)
    } else if desktop.contains("mate") {
        run(
            "org.mate.background",
            "picture-filename",
            path.to_str().ok_or("Non UTF-8 path")?,
        )
    } else {
        Err(
            "This Linux desktop is not supported yet. MVP supports GNOME, Cinnamon and MATE."
                .into(),
        )
    }
}
