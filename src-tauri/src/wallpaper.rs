use crate::{model::Pin, network};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};
const MAX_DOWNLOAD: u64 = 25 * 1024 * 1024;
pub const MAX_FOREGROUND_ATTEMPTS: usize = 8;
pub const FOREGROUND_SEARCH_BUDGET: Duration = Duration::from_secs(90);
pub fn quality_cache(dir: &Path, pin: &Pin) -> PathBuf {
    dir.join(format!(
        "lossless-v2-{:x}.png",
        Sha256::digest(pin.url.as_bytes())
    ))
}
pub fn cached(dir: &Path, pin: &Pin) -> PathBuf {
    let upgraded = quality_cache(dir, pin);
    if upgraded.exists() {
        upgraded
    } else {
        dir.join(format!("{:x}.jpg", Sha256::digest(pin.url.as_bytes())))
    }
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
#[derive(Debug)]
pub struct SelectionFailure {
    pub attempts: usize,
    pub max_attempts: usize,
    pub candidates: usize,
    pub exhausted_budget: bool,
    pub errors: Vec<String>,
}

/// Try reusable candidates first, then download candidates one at a time with
/// a finite attempt/time budget. Candidate keys are deduplicated before any
/// preparation work so a failing pin or URL cannot be retried in one click.
pub fn select_bounded<P, T>(
    pins: impl IntoIterator<Item = P>,
    mut ready: impl FnMut(&P) -> bool,
    mut candidate_keys: impl FnMut(&P) -> (String, String),
    max_attempts: usize,
    time_budget: Duration,
    mut on_attempt: impl FnMut(usize, &P),
    mut on_failure: impl FnMut(usize, &P, &str),
    mut prepare: impl FnMut(P) -> Result<T, String>,
) -> Result<T, SelectionFailure>
where
    P: Clone,
{
    let mut ids = HashSet::new();
    let mut urls = HashSet::new();
    let candidates: Vec<P> = pins
        .into_iter()
        .filter(|pin| {
            let (id, url) = candidate_keys(pin);
            ids.insert(id) && urls.insert(url)
        })
        .collect();
    let candidate_count = candidates.len();
    let mut reusable = Vec::new();
    let mut missing = Vec::new();
    for pin in candidates {
        if ready(&pin) {
            reusable.push(pin);
        } else {
            missing.push(pin);
        }
    }
    let ordered = reusable.into_iter().chain(missing).collect::<Vec<_>>();
    let deadline = Instant::now() + time_budget;
    let mut errors = Vec::new();
    let mut attempts = 0;
    let mut exhausted_budget = max_attempts == 0 || time_budget.is_zero();
    for pin in &ordered {
        if exhausted_budget
            || attempts >= max_attempts
            || (attempts > 0 && Instant::now() >= deadline)
        {
            exhausted_budget = true;
            break;
        }
        attempts += 1;
        on_attempt(attempts, pin);
        match prepare(pin.clone()) {
            Ok(result) => return Ok(result),
            Err(error) => {
                on_failure(attempts, pin, &error);
                errors.push(error);
            }
        }
    }
    Err(SelectionFailure {
        attempts,
        max_attempts,
        candidates: candidate_count,
        exhausted_budget,
        errors,
    })
}
#[cfg(test)]
fn first_usable<P, T>(
    pins: impl IntoIterator<Item = P>,
    mut prepare: impl FnMut(P) -> Result<T, String>,
) -> Result<T, String> {
    let mut last = "No matching pictures".to_owned();
    for pin in pins {
        match prepare(pin) {
            Ok(candidate) => return Ok(candidate),
            Err(error) => last = error,
        }
    }
    Err(format!("No suitable downloadable pictures remain. {last}"))
}
fn failures() -> &'static Mutex<HashMap<String, std::time::Instant>> {
    static FAILURES: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    FAILURES.get_or_init(Default::default)
}
pub fn temporarily_unavailable(pin: &Pin) -> bool {
    let mut failures = failures().lock().unwrap();
    failures.retain(|_, since| since.elapsed() < std::time::Duration::from_secs(300));
    failures.contains_key(&pin.url)
}
pub fn download(dir: &Path, pin: &Pin) -> Result<PathBuf, String> {
    if !network::valid_image_url(&pin.url) {
        return Err("Image is not hosted on Pinterest's image CDN".into());
    }
    fs::create_dir_all(dir).map_err(|_| "Cannot create image cache")?;
    let path = quality_cache(dir, pin);
    let result = cache_once(&path, || {
        let client = network::client()?;
        let original = network::original_url(&pin.url);
        let mut response = client.get(&original).send().map_err(|error| {
            if error.is_timeout() {
                "Image download timed out after 30 seconds"
            } else {
                "Image download failed"
            }
        })?;
        // Fall back only when the preferred URL is absent, never because of a
        // timeout. Every fallback was either imported from the page or is the
        // exact source URL supplied by the page; no URL variants are invented.
        if matches!(response.status().as_u16(), 403 | 404 | 410) {
            let mut fallbacks = Vec::new();
            for candidate in [Some(pin.url.as_str()), pin.fallback_url.as_deref()] {
                if let Some(candidate) =
                    candidate.filter(|url| *url != original && network::valid_image_url(url))
                {
                    if !fallbacks.iter().any(|seen| seen == &candidate) {
                        fallbacks.push(candidate);
                    }
                }
            }
            for candidate in fallbacks {
                response = client
                    .get(candidate)
                    .send()
                    .map_err(|_| "Image fallback download failed")?;
                if response.status().is_success()
                    || !matches!(response.status().as_u16(), 403 | 404 | 410)
                {
                    break;
                }
            }
        }
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
        let decoded = decode_oriented(bytes)?;
        let temp = path.with_extension("tmp");
        // Lossless, but use fast compression: wallpaper changes must not wait
        // for a high-compression PNG pass over millions of pixels.
        let file = std::fs::File::create(&temp).map_err(|_| "Cannot write image cache")?;
        let encoder = image::codecs::png::PngEncoder::new_with_quality(
            std::io::BufWriter::new(file),
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Sub,
        );
        decoded
            .write_with_encoder(encoder)
            .map_err(|_| "Cannot encode cached image")?;
        if fs::metadata(&temp)
            .map_err(|_| "Cannot inspect cached image")?
            .len()
            > 256 * 1024 * 1024
        {
            let _ = fs::remove_file(&temp);
            return Err("Decoded PNG exceeds 256 MB".into());
        }
        fs::rename(&temp, &path).map_err(|_| "Cannot finish cached image")?;
        let _ = crate::preview::prepare(&path, &decoded);
        Ok(path.clone())
    });
    if result.is_err() {
        failures()
            .lock()
            .unwrap()
            .insert(pin.url.clone(), std::time::Instant::now());
    }
    result
}
fn decode_oriented(bytes: Vec<u8>) -> Result<image::DynamicImage, String> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Unknown image format")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| "Image cannot be decoded within safety limits")?;
    let orientation = decoder
        .orientation()
        .map_err(|_| "Cannot read image orientation")?;
    let mut decoded =
        image::DynamicImage::from_decoder(decoder).map_err(|_| "Image cannot be decoded")?;
    decoded.apply_orientation(orientation);
    Ok(decoded)
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
            if workspace
                .desktopImageURLForScreen(&screen)
                .and_then(|current| current.path())
                .is_some_and(|current| current.to_string() == path)
            {
                continue;
            }
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
    fn exif_rotation_changes_dimensions_and_png_preserves_pixels() {
        let source = image::RgbImage::from_fn(8, 4, |x, y| {
            image::Rgb([(x * 20) as u8, (y * 40) as u8, 70])
        });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100)
            .encode_image(&image::DynamicImage::ImageRgb8(source))
            .unwrap();
        // Minimal little-endian TIFF: orientation=6 (90 degrees clockwise).
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut tagged = jpeg[..2].to_vec();
        tagged.extend_from_slice(&[0xff, 0xe1]);
        tagged.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        tagged.extend_from_slice(exif);
        tagged.extend_from_slice(&jpeg[2..]);
        let decoded = decode_oriented(tagged).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (4, 8));
        let mut png = std::io::Cursor::new(Vec::new());
        decoded.write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert_eq!(
            decode_oriented(png.into_inner()).unwrap().to_rgb8(),
            decoded.to_rgb8()
        );
    }
    #[test]
    fn selection_passes_five_rejections_and_stops_at_first_usable_picture() {
        let mut visited = Vec::new();
        let found = first_usable(0..10, |n| {
            visited.push(n);
            if n < 7 {
                Err("too small".into())
            } else {
                Ok(n)
            }
        })
        .unwrap();
        assert_eq!(found, 7);
        assert_eq!(visited, (0..8).collect::<Vec<_>>());
        let mut count = 0;
        assert!(first_usable(0..9, |_| {
            count += 1;
            Err::<(), _>("unavailable".into())
        })
        .is_err());
        assert_eq!(count, 9);
    }

    #[test]
    fn bounded_selection_advances_after_a_failed_download_and_reports_progress() {
        let mut attempts = Vec::new();
        let mut failures = Vec::new();
        let result = select_bounded(
            0..4,
            |_| false,
            |n| (n.to_string(), n.to_string()),
            4,
            Duration::from_secs(5),
            |attempt, pin| attempts.push((attempt, *pin)),
            |attempt, pin, error| failures.push((attempt, *pin, error.to_owned())),
            |pin| {
                if pin < 2 {
                    Err(format!("download failed for {pin}"))
                } else {
                    Ok(pin)
                }
            },
        )
        .unwrap();
        assert_eq!(result, 2);
        assert_eq!(attempts, vec![(1, 0), (2, 1), (3, 2)]);
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].0, 1);
        assert_eq!(failures[1].1, 1);
    }

    #[test]
    fn bounded_selection_stops_at_attempt_budget_without_crawling_library() {
        let mut attempts = Vec::new();
        let failure = select_bounded(
            0..100,
            |_| false,
            |n| (n.to_string(), n.to_string()),
            3,
            Duration::from_secs(5),
            |_, pin| attempts.push(*pin),
            |_, _, _| {},
            |_| Err::<(), _>("unavailable".into()),
        )
        .unwrap_err();
        assert_eq!(attempts, vec![0, 1, 2]);
        assert_eq!(failure.attempts, 3);
        assert!(failure.exhausted_budget);
        assert_eq!(failure.candidates, 100);
    }

    #[test]
    fn bounded_selection_deduplicates_pin_and_url_keys() {
        let mut attempts = Vec::new();
        let result = select_bounded(
            vec![
                ("pin-a", "url-a"),
                ("pin-a", "url-a-variant"),
                ("pin-b", "url-a"),
                ("pin-c", "url-c"),
            ],
            |_| false,
            |(pin, url)| (pin.to_string(), url.to_string()),
            5,
            Duration::from_secs(5),
            |_, candidate| attempts.push(*candidate),
            |_, _, _| {},
            |candidate| {
                if candidate.0 == "pin-c" {
                    Ok(candidate.0)
                } else {
                    Err("refused".into())
                }
            },
        )
        .unwrap();
        assert_eq!(result, "pin-c");
        assert_eq!(attempts, vec![("pin-a", "url-a"), ("pin-c", "url-c")]);
    }

    #[test]
    fn bounded_selection_reports_a_zero_budget_without_attempting() {
        let mut attempts = 0;
        let failure = select_bounded(
            0..10,
            |_| false,
            |n| (n.to_string(), n.to_string()),
            8,
            Duration::ZERO,
            |_, _| attempts += 1,
            |_, _, _| {},
            |_| Err::<(), _>("network unavailable".into()),
        )
        .unwrap_err();
        assert_eq!(attempts, 0);
        assert!(failure.exhausted_budget);
        assert_eq!(failure.attempts, 0);
    }
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
