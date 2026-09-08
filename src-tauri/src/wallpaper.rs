use crate::{model::Pin, network};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
const MAX_DOWNLOAD: u64 = 25 * 1024 * 1024;
const CACHE_LIMIT: u64 = 256 * 1024 * 1024;
pub fn cached(dir: &Path, pin: &Pin) -> PathBuf {
    dir.join(format!("{:x}.jpg", Sha256::digest(pin.url.as_bytes())))
}
// Shared by foreground and prefetch: one writer per URL, with a cache
// recheck after acquiring the lock. Different URLs never block each other.
fn download_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap();
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_owned(), Arc::downgrade(&lock));
    lock
}
fn cache_once(
    path: &Path,
    load: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    let lock = download_lock(path);
    let _writer = lock.lock().unwrap();
    if path.exists() {
        return Ok(path.to_owned());
    }
    load()
}
pub fn has_prefetch_room(dir: &Path) -> bool {
    let total: u64 = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    total < CACHE_LIMIT.saturating_sub(MAX_DOWNLOAD)
}
pub fn download(dir: &Path, pin: &Pin) -> Result<PathBuf, String> {
    if !network::valid_image_url(&pin.url) {
        return Err("Image is not hosted on Pinterest's image CDN".into());
    }
    fs::create_dir_all(dir).map_err(|_| "Cannot create image cache")?;
    let path = cached(dir, pin);
    cache_once(&path, || {
        let response = network::client()?.get(&pin.url).send().map_err(|error| {
            if error.is_timeout() {
                "Image download timed out after 30 seconds"
            } else {
                "Image download failed"
            }
        })?;
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
        if fs::metadata(&temp)
            .map_err(|_| "Cannot inspect cached image")?
            .len()
            > MAX_DOWNLOAD
        {
            let _ = fs::remove_file(&temp);
            return Err("Converted image exceeds 25 MB".into());
        }
        fs::rename(&temp, &path).map_err(|_| "Cannot finish cached image")?;
        Ok(path.clone())
    })
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

#[cfg(test)]
mod download_tests {
    use super::*;
    #[test]
    fn concurrent_foreground_and_prefetch_publish_once_and_failed_load_can_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir =
            std::env::temp_dir().join(format!("pinpaper-cache-test-{}", rand::random::<u64>()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("image.jpg");
        let writes = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (path, writes, barrier) = (&path, &writes, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    cache_once(path, || {
                        writes.fetch_add(1, Ordering::SeqCst);
                        fs::write(path, b"complete image").unwrap();
                        Ok(path.clone())
                    })
                    .unwrap();
                    assert_eq!(fs::read(path).unwrap(), b"complete image");
                });
            }
        });
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        let retry = dir.join("retry.jpg");
        assert!(cache_once(&retry, || Err("temporary failure".into())).is_err());
        cache_once(&retry, || {
            fs::write(&retry, b"retried").unwrap();
            Ok(retry.clone())
        })
        .unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
