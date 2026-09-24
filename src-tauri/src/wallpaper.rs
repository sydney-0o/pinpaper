use crate::{model::Pin, network};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{Duration, Instant},
};
const MAX_DOWNLOAD: u64 = 25 * 1024 * 1024;
/// A foreground change can be stopped explicitly from the UI. This sentinel
/// lets the selection loop return a neutral cancellation result instead of
/// treating the user's stop request as a failed image.
pub const CHANGE_CANCELLED: &str = "Wallpaper change stopped";
/// A foreground change applies one wallpaper, so its success target is one
/// successfully prepared/saved image. The retry budget is separate: it is the
/// finite set of distinct saved-picture candidates in the current round.
pub const FOREGROUND_TARGET_SUCCESSES: usize = 1;
// Each network request remains bounded by reqwest's client timeout. The
// foreground search itself has no arbitrary attempt ceiling: its finite input
// is the currently selected set of saved pictures, with each distinct image
// inspected at most once per rotation round.
pub const FOREGROUND_SEARCH_BUDGET: Duration = Duration::MAX;
fn cache_identity(pin: &Pin) -> String {
    match pin.source_url.as_deref() {
        Some(source) => format!("{}\0{}", pin.url, source),
        None => pin.url.clone(),
    }
}
pub fn quality_cache(dir: &Path, pin: &Pin) -> PathBuf {
    dir.join(format!(
        "lossless-v2-{:x}.png",
        Sha256::digest(cache_identity(pin).as_bytes())
    ))
}
pub fn cached(dir: &Path, pin: &Pin) -> PathBuf {
    let upgraded = quality_cache(dir, pin);
    // A decoded thumbnail is not evidence that the original was attempted.
    // Both foreground and prefetch prepare metadata before using this cache.
    if network::needs_pin_resolution(pin) {
        return upgraded.with_extension("pending");
    }
    if pin.source_url.is_some() {
        // A Pinterest fallback written after a temporary source outage is
        // intentionally retried later. Reuse this cache only after the
        // persisted dimensions provenance proves that its pixels came from
        // the external image URL.
        if upgraded.exists()
            && pin.dimensions_url.as_deref().is_some_and(|url| {
                network::valid_source_url(url) && !network::valid_image_url(url)
            })
        {
            return upgraded;
        }
        if upgraded.exists() && network::external_source_backoff(pin) {
            // The external page/image was already tried recently and failed
            // or was rejected as ambiguous. Reuse the decoded Pinterest
            // fallback during the short negative-cache window.
            return upgraded;
        }
        if upgraded.exists() {
            // Keep the fallback file on disk for recovery/diagnostics, but do
            // not expose it as a ready candidate while the source URL can be
            // retried.
            return upgraded.with_extension("retry");
        }
        return upgraded;
    }
    if upgraded.exists() {
        upgraded
    } else {
        dir.join(format!("{:x}.jpg", Sha256::digest(pin.url.as_bytes())))
    }
}

/// Read the dimensions of the image stored on disk, including the EXIF
/// orientation that is applied to legacy JPEG caches. This is the only
/// dimension source used after a download: Pinterest page metadata never
/// overrides pixels that were actually decoded from the cache.
pub fn decoded_dimensions(path: &Path) -> Result<(u32, u32), String> {
    use image::ImageDecoder;

    let mut reader = image::ImageReader::open(path)
        .map_err(|_| "Cached image could not be read")?
        .with_guessed_format()
        .map_err(|_| "Unknown cached image format")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| "Cached image could not be decoded")?;
    let orientation = decoder
        .orientation()
        .map_err(|_| "Cached image orientation could not be read")?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err("Cached image exceeds safety limits".into());
    }
    let swaps_axes = matches!(
        orientation,
        image::metadata::Orientation::Rotate90
            | image::metadata::Orientation::Rotate270
            | image::metadata::Orientation::Rotate90FlipH
            | image::metadata::Orientation::Rotate270FlipH
    );
    Ok(if swaps_axes {
        (height, width)
    } else {
        (width, height)
    })
}

pub fn cached_dimensions(dir: &Path, pin: &Pin) -> Option<(u32, u32)> {
    // Inspection still reports the real bytes of a cached fallback while a
    // quality upgrade is pending. Selection uses `cached` separately.
    let current = quality_cache(dir, pin);
    let path = if current.exists() { current } else {
        dir.join(format!("{:x}.jpg", Sha256::digest(pin.url.as_bytes())))
    };
    path.exists()
        .then(|| decoded_dimensions(&path).ok())
        .flatten()
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

/// Coordinates the small commit/cleanup section of cache operations. Network
/// requests and image decoding happen outside this lock, so resetting a large
/// cache never waits for a download to finish before the UI can start its
/// background operation.
pub fn cache_gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

/// Invalidate all older background cache jobs and remove the image cache as a
/// single reset-safe operation. The generation is advanced while holding the
/// same gate used by `download_with_guard`, so a job either commits before the
/// reset and is removed here, or observes the new generation and commits
/// nothing.
pub fn reset_cache(dir: &Path, generation: &AtomicU64) -> Result<(), String> {
    let _gate = cache_gate().lock().map_err(|_| "Image cache is busy")?;
    generation.fetch_add(1, Ordering::SeqCst);
    cache_sources().lock().unwrap().clear();
    network::clear_external_source_cache();
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|error| {
            format!("Reset completed except image cache cleanup failed: {error}")
        })?;
    }
    Ok(())
}
#[cfg(test)]
fn cache_once(
    path: &Path,
    load: impl FnOnce() -> Result<(PathBuf, String), String>,
) -> Result<(PathBuf, String), String> {
    cache_once_with_policy(path, true, load)
}

fn cache_once_with_policy(
    path: &Path,
    reuse_existing: bool,
    load: impl FnOnce() -> Result<(PathBuf, String), String>,
) -> Result<(PathBuf, String), String> {
    let lock = download_lock(path);
    let _writer = lock.lock().unwrap();
    if reuse_existing && path.exists() {
        let source_url = cache_sources()
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or_default();
        return Ok((path.to_owned(), source_url));
    }
    let result = load();
    if let Ok((cached_path, source_url)) = &result {
        cache_sources()
            .lock()
            .unwrap()
            .insert(cached_path.clone(), source_url.clone());
    }
    result
}

fn cache_sources() -> &'static Mutex<HashMap<PathBuf, String>> {
    static SOURCES: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    SOURCES.get_or_init(Default::default)
}
#[derive(Debug)]
pub struct SelectionFailure {
    pub attempts: usize,
    pub max_attempts: usize,
    pub candidates: usize,
    pub successful: usize,
    pub target_successes: usize,
    pub exhausted_budget: bool,
    pub source_exhausted: bool,
    pub cancelled: bool,
    pub errors: Vec<String>,
}

#[derive(Debug)]
pub struct DownloadedImage {
    pub path: PathBuf,
    pub source_url: String,
}

/// Try candidates one at a time until the success target is reached or the
/// finite candidate source/budget is exhausted. Candidate keys are
/// deduplicated before any preparation work so a failing pin or URL cannot be
/// retried in one click. Successful results are retained so callers can use a
/// target larger than one without changing the retry semantics.
fn select_bounded_with_order<P, T>(
    pins: impl IntoIterator<Item = P>,
    mut ready: impl FnMut(&P) -> bool,
    mut candidate_keys: impl FnMut(&P) -> (String, String),
    max_attempts: usize,
    target_successes: usize,
    time_budget: Duration,
    mut on_attempt: impl FnMut(usize, &P),
    mut on_failure: impl FnMut(usize, &P, &str),
    mut prepare: impl FnMut(P) -> Result<T, String>,
    prefer_ready: bool,
) -> Result<Vec<T>, SelectionFailure>
where
    P: Clone,
{
    if target_successes == 0 {
        return Ok(Vec::new());
    }
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
    let ordered = if prefer_ready {
        let mut reusable = Vec::new();
        let mut missing = Vec::new();
        for pin in candidates {
            if ready(&pin) {
                reusable.push(pin);
            } else {
                missing.push(pin);
            }
        }
        reusable.into_iter().chain(missing).collect::<Vec<_>>()
    } else {
        candidates
    };
    // `Duration::MAX` deliberately disables the overall wall-clock cutoff.
    // Keep a finite budget available for unit tests and callers that need one,
    // while avoiding an Instant overflow for the unbounded-by-time production
    // search.
    let deadline = if time_budget.is_zero() {
        Some(Instant::now())
    } else {
        Instant::now().checked_add(time_budget)
    };
    let mut errors = Vec::new();
    let mut attempts = 0;
    let mut successful = Vec::new();
    let mut exhausted_budget = max_attempts == 0 || time_budget.is_zero();
    for pin in &ordered {
        if exhausted_budget
            || attempts >= max_attempts
            || deadline.is_some_and(|deadline| attempts > 0 && Instant::now() >= deadline)
        {
            exhausted_budget = true;
            break;
        }
        attempts += 1;
        on_attempt(attempts, pin);
        match prepare(pin.clone()) {
            Ok(result) => {
                successful.push(result);
                if successful.len() >= target_successes {
                    return Ok(successful);
                }
            }
            Err(error) => {
                if error == CHANGE_CANCELLED {
                    return Err(SelectionFailure {
                        attempts,
                        // `usize::MAX` is the unbounded production mode. The
                        // meaningful limit shown to the user is the number of
                        // distinct saved-picture candidates in this round.
                        max_attempts: if max_attempts == usize::MAX {
                            candidate_count
                        } else {
                            max_attempts
                        },
                        candidates: candidate_count,
                        successful: successful.len(),
                        target_successes,
                        exhausted_budget: false,
                        source_exhausted: false,
                        cancelled: true,
                        errors: Vec::new(),
                    });
                }
                on_failure(attempts, pin, &error);
                errors.push(error);
            }
        }
    }
    let source_exhausted = !exhausted_budget && attempts >= ordered.len();
    Err(SelectionFailure {
        attempts,
        // The foreground call uses `usize::MAX` as a marker for an
        // unbounded-by-attempts search. Report the finite candidate count so
        // progress and diagnostics describe the actual saved-picture limit.
        max_attempts: if max_attempts == usize::MAX {
            candidate_count
        } else {
            max_attempts
        },
        candidates: candidate_count,
        successful: successful.len(),
        target_successes,
        exhausted_budget,
        source_exhausted,
        cancelled: false,
        errors,
    })
}

/// Try reusable candidates first, then download candidates one at a time.
/// This is kept for callers whose input order is not a rotation priority.
#[cfg(test)]
fn select_bounded<P, T>(
    pins: impl IntoIterator<Item = P>,
    ready: impl FnMut(&P) -> bool,
    candidate_keys: impl FnMut(&P) -> (String, String),
    max_attempts: usize,
    time_budget: Duration,
    on_attempt: impl FnMut(usize, &P),
    on_failure: impl FnMut(usize, &P, &str),
    prepare: impl FnMut(P) -> Result<T, String>,
) -> Result<T, SelectionFailure>
where
    P: Clone,
{
    select_bounded_with_order(
        pins,
        ready,
        candidate_keys,
        max_attempts,
        1,
        time_budget,
        on_attempt,
        on_failure,
        prepare,
        true,
    )
    .map(|mut results| {
        results
            .pop()
            .expect("a one-success selection must return one result")
    })
}

/// Run the foreground wallpaper search with its production retry policy.
///
/// Keeping this policy at the call boundary prevents a foreground caller from
/// accidentally inheriting the background worker's two-item warm-up limit.
/// Each attempt still represents one distinct image candidate; a candidate's
/// original URL and its observed fallback are handled inside `download`.
#[cfg(test)]
pub fn select_foreground<P, T>(
    pins: impl IntoIterator<Item = P>,
    ready: impl FnMut(&P) -> bool,
    candidate_keys: impl FnMut(&P) -> (String, String),
    on_attempt: impl FnMut(usize, &P),
    on_failure: impl FnMut(usize, &P, &str),
    prepare: impl FnMut(P) -> Result<T, String>,
) -> Result<T, SelectionFailure>
where
    P: Clone,
{
    select_foreground_with_target(
        pins,
        FOREGROUND_TARGET_SUCCESSES,
        ready,
        candidate_keys,
        on_attempt,
        on_failure,
        prepare,
    )
    .map(|mut results| {
        results
            .pop()
            .expect("a one-success foreground selection must return one result")
    })
}

/// Run a foreground search with an explicit number of successful prepared
/// images as its goal. The current wallpaper-change command passes one because
/// it applies one wallpaper per invocation; tests and future batch callers can
/// use a larger target without reintroducing an attempt ceiling.
pub fn select_foreground_with_target<P, T>(
    pins: impl IntoIterator<Item = P>,
    target_successes: usize,
    ready: impl FnMut(&P) -> bool,
    candidate_keys: impl FnMut(&P) -> (String, String),
    on_attempt: impl FnMut(usize, &P),
    on_failure: impl FnMut(usize, &P, &str),
    prepare: impl FnMut(P) -> Result<T, String>,
) -> Result<Vec<T>, SelectionFailure>
where
    P: Clone,
{
    select_bounded_with_order(
        pins,
        ready,
        candidate_keys,
        // There is no fixed retry ceiling anymore. The helper still receives
        // a finite vector and therefore visits no more than the number of
        // distinct saved-picture candidates supplied for this change.
        usize::MAX,
        target_successes,
        FOREGROUND_SEARCH_BUDGET,
        on_attempt,
        on_failure,
        prepare,
        false,
    )
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

/// A deliberate foreground retry may immediately revisit matching URLs that a
/// background warm-up marked as unavailable. Keep the reset scoped to the
/// current candidate set so unrelated libraries are not affected.
pub fn clear_temporary_unavailable<'a>(pins: impl IntoIterator<Item = &'a Pin>) {
    let mut failures = failures().lock().unwrap();
    for pin in pins {
        failures.remove(&pin.url);
    }
}

/// A local-data reset starts a new library, so URL failure cooldowns from the
/// old library must not affect a later import of the same Pinterest pin.
pub fn clear_all_temporary_unavailable() {
    failures().lock().unwrap().clear();
}

/// Build the candidates for one foreground change. Manual changes explicitly
/// bypass the background failure cooldown, while scheduled changes continue to
/// honor it. Cached images remain eligible in either mode.
pub fn foreground_candidates(
    pins: impl IntoIterator<Item = Pin>,
    cache: &Path,
    manual_retry: bool,
) -> Vec<Pin> {
    pins.into_iter()
        .filter(|pin| {
            cached(cache, pin).exists()
                || manual_retry
                || !temporarily_unavailable(pin)
                // A legacy /originals/ URL needs the bounded page repair in
                // the foreground path and must not be hidden by prefetch state.
                || (pin.fallback_url.is_none()
                    && network::is_original_url(&pin.url)
                    && !pin.original_url_exact)
        })
        .collect()
}

fn decoded_image_is_better(candidate: &image::DynamicImage, current: &image::DynamicImage) -> bool {
    let candidate_area = u64::from(candidate.width())
        .saturating_mul(u64::from(candidate.height()));
    let current_area = u64::from(current.width()).saturating_mul(u64::from(current.height()));
    candidate_area > current_area
        || (candidate_area == current_area && candidate.width() > current.width())
}

/// Download an image and commit it only while `can_commit` is still true.
/// Callers use this for background work that may outlive a local-data reset.
/// The callback is checked while holding the same gate used by reset, making a
/// generation change and cache cleanup mutually exclusive with the final
/// rename and preview write.
pub fn download_with_guard(
    dir: &Path,
    pin: &Pin,
    can_commit: impl Fn() -> bool,
) -> Result<DownloadedImage, String> {
    if !network::valid_image_url(&pin.url) {
        return Err("Image is not hosted on Pinterest's image CDN".into());
    }
    let path = quality_cache(dir, pin);
    let reuse_existing = pin.source_url.is_none()
        || network::external_source_backoff(pin)
        || pin.dimensions_url.as_deref().is_some_and(|url| {
            network::valid_source_url(url) && !network::valid_image_url(url)
        });
    let result = cache_once_with_policy(&path, reuse_existing, || {
        let client = network::client()?;
        let preferred = network::preferred_download_url(pin);
        let guessed_original_has_fallback = network::is_original_url(&pin.url)
            && !pin.original_url_exact
            && pin.fallback_url.is_some();
        let external = network::resolve_external_image(pin).map(|resolved| resolved.image_url);
        if !can_commit() {
            return Err("Image download cancelled by local-data reset".into());
        }
        let mut candidates: Vec<(String, bool)> = Vec::new();
        if let Some(url) = external {
            candidates.push((url, true));
        }
        for candidate in [
            preferred,
            pin.url.clone(),
            pin.fallback_url.clone().unwrap_or_default(),
        ] {
            if candidate.is_empty()
                || (guessed_original_has_fallback && candidate == pin.url)
                || !network::valid_image_url(&candidate)
                || !network::same_asset(&candidate, &pin.url)
                || candidates.iter().any(|(seen, _)| seen == &candidate)
            {
                continue;
            }
            candidates.push((candidate, false));
        }
        let mut last_error = "Image download failed".to_owned();
        let mut selected: Option<(image::DynamicImage, String, bool)> = None;
        let mut external_attempted = false;
        for (candidate, is_external) in candidates {
            if !can_commit() {
                return Err("Image download cancelled by local-data reset".into());
            }
            if is_external {
                external_attempted = true;
            }
            let response = if is_external {
                let source_client = match network::source_client_for_url(&candidate) {
                    Ok(client) => client,
                    Err(_) => {
                        last_error = "External source address was not public".into();
                        continue;
                    }
                };
                source_client.get(&candidate).send()
            } else {
                client.get(&candidate).send()
            };
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    last_error = if error.is_timeout() {
                        "Image download timed out after 30 seconds".into()
                    } else {
                        "Image download failed".into()
                    };
                    // A source page/image is optional. Pinterest remains the
                    // authoritative fallback when the external host is down.
                    if is_external {
                        continue;
                    }
                    break;
                }
            };
            if !response.status().is_success() {
                last_error = format!("Image download returned HTTP {}", response.status());
                if is_external || matches!(response.status().as_u16(), 403 | 404 | 410) {
                    continue;
                }
                break;
            }
            if response.content_length().unwrap_or(0) > MAX_DOWNLOAD {
                last_error = "Image exceeds 25 MB".into();
                if is_external {
                    continue;
                }
                break;
            }
            let mut bytes = vec![];
            if response
                .take(MAX_DOWNLOAD + 1)
                .read_to_end(&mut bytes)
                .is_err()
            {
                last_error = "Image download interrupted".into();
                if is_external {
                    continue;
                }
                break;
            }
            if bytes.len() as u64 > MAX_DOWNLOAD {
                last_error = "Image exceeds 25 MB".into();
                if is_external {
                    continue;
                }
                break;
            }
            if !can_commit() {
                return Err("Image download cancelled by local-data reset".into());
            }
            match decode_oriented(bytes) {
                Ok(decoded) => {
                    let replace = selected
                        .as_ref()
                        .map(|(current, _, _)| decoded_image_is_better(&decoded, current))
                        .unwrap_or(true);
                    if replace {
                        selected = Some((decoded, candidate, is_external));
                    }
                    // The first successfully decoded Pinterest candidate is
                    // already the observed preferred variant (or its supplied
                    // fallback). There is no quality benefit in downloading
                    // the same asset's lower CDN variants after that baseline;
                    // the comparison above has already decided whether the
                    // external file wins.
                    if !is_external {
                        break;
                    }
                }
                Err(error) => {
                    last_error = error;
                    if is_external {
                        continue;
                    }
                    break;
                }
            }
        }
        if external_attempted
            && selected
                .as_ref()
                .map(|(_, _, is_external)| !*is_external)
                .unwrap_or(true)
        {
            // Remember a source request that could not provide a better
            // decoded image. The Pinterest fallback stays reusable for a
            // short TTL instead of causing a page walk on every wallpaper
            // change.
            network::mark_external_source_unavailable(pin);
        }
        let Some((decoded, source_url, _)) = selected else {
            return Err(last_error);
        };
        // Do not create the cache directory until the reset-safe commit
        // section. An old prefetch job can therefore finish its request after
        // reset without recreating an empty cache or leaving a thumbnail.
        let _gate = cache_gate().lock().map_err(|_| "Image cache unavailable")?;
        if !can_commit() {
            return Err("Image download cancelled by local-data reset".into());
        }
        if !reuse_existing {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(path.with_extension("preview.jpg"));
        }
        fs::create_dir_all(dir).map_err(|_| "Cannot create image cache")?;
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
        if !can_commit() {
            let _ = fs::remove_file(&temp);
            return Err("Image download cancelled by local-data reset".into());
        }
        fs::rename(&temp, &path).map_err(|_| "Cannot finish cached image")?;
        if !can_commit() {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(path.with_extension("preview.jpg"));
            return Err("Image download cancelled by local-data reset".into());
        }
        let _ = crate::preview::prepare(&path, &decoded);
        Ok((path.clone(), source_url))
    });
    if result
        .as_ref()
        .err()
        .is_some_and(|error| !error.contains("cancelled by local-data reset"))
    {
        failures()
            .lock()
            .unwrap()
            .insert(pin.url.clone(), std::time::Instant::now());
    }
    result.map(|(path, source_url)| DownloadedImage { path, source_url })
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
fn tiled_wallpaper(path: &Path, width: u32, height: u32) -> Result<PathBuf, String> {
    let source = image::open(path)
        .map_err(|_| "Could not decode the wallpaper for tiling".to_owned())?
        .to_rgba8();
    if source.width() == 0 || source.height() == 0 {
        return Err("Wallpaper has no pixels to tile".into());
    }
    let width = width.min(16384);
    let height = height.min(16384);
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("Tiled wallpaper is too large")?;
    if pixels > 256 * 1024 * 1024 {
        return Err("Tiled wallpaper exceeds 256 MB".into());
    }
    let key = format!("{}:{}x{}", path.to_string_lossy(), width, height);
    let output = path
        .parent()
        .ok_or("Wallpaper cache has no parent directory")?
        .join(format!(
            "tile-v1-{:x}-{}x{}.png",
            Sha256::digest(key),
            width,
            height
        ));
    if output.exists() {
        return Ok(output);
    }
    let mut tiled = image::RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            tiled.put_pixel(
                x,
                y,
                *source.get_pixel(x % source.width(), y % source.height()),
            );
        }
    }
    let temp = output.with_extension("tmp");
    let file = fs::File::create(&temp).map_err(|_| "Cannot write tiled wallpaper cache")?;
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        std::io::BufWriter::new(file),
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Sub,
    );
    image::DynamicImage::ImageRgba8(tiled)
        .write_with_encoder(encoder)
        .map_err(|_| "Cannot encode tiled wallpaper")?;
    fs::rename(&temp, &output).map_err(|_| "Cannot finish tiled wallpaper cache")?;
    Ok(output)
}

#[cfg(target_os = "macos")]
pub fn apply(app: &tauri::AppHandle, path: &Path, display_mode: &str) -> Result<(), String> {
    let path = path.to_path_buf();
    let display_mode = display_mode.to_owned();
    let perform = move || -> Result<(), String> {
        use objc2::runtime::AnyObject;
        use objc2::MainThreadMarker;
        use objc2_app_kit::{
            NSScreen, NSWorkspace, NSWorkspaceDesktopImageAllowClippingKey,
            NSWorkspaceDesktopImageScalingKey,
        };
        use objc2_foundation::{NSMutableDictionary, NSNumber, NSString, NSURL};
        let main =
            MainThreadMarker::new().ok_or("Wallpaper update requires the macOS main thread")?;
        let workspace = NSWorkspace::sharedWorkspace();
        let screens = NSScreen::screens(main);
        if screens.is_empty() {
            return Err("No connected display found".into());
        }
        for screen in screens.iter() {
            let screen_path = if display_mode == "tile" {
                let frame = screen.frame();
                let scale = screen.backingScaleFactor().max(1.0);
                let width = (frame.size.width * scale).round() as u32;
                let height = (frame.size.height * scale).round() as u32;
                tiled_wallpaper(&path, width, height)?
            } else {
                path.clone()
            };
            let path_text = screen_path.to_str().ok_or("Invalid wallpaper file path")?;
            let url = NSURL::fileURLWithPath(&NSString::from_str(path_text));
            if workspace
                .desktopImageURLForScreen(&screen)
                .and_then(|current| current.path())
                .is_some_and(|current| current.to_string() == path_text)
            {
                continue;
            }
            let options = workspace
                .desktopImageOptionsForScreen(&screen)
                .map(|existing| NSMutableDictionary::dictionaryWithDictionary(&existing))
                .unwrap_or_else(NSMutableDictionary::new);
            let (scaling, clipping) = match display_mode.as_str() {
                // NSImageScaleProportionallyUpOrDown = 3.
                "fit" => (3, false),
                "center" | "tile" => (2, false), // NSImageScaleNone = 2.
                // NSImageScaleAxesIndependently = 1.
                "stretch" => (1, false),
                _ => (3, true),
            };
            let scaling = NSNumber::new_isize(scaling);
            let clipping = NSNumber::new_bool(clipping);
            let scaling_object: &AnyObject = scaling.as_ref();
            let clipping_object: &AnyObject = clipping.as_ref();
            // objc2 exposes these AppKit option keys as extern statics. They are
            // immutable process-wide constants owned by AppKit.
            unsafe {
                options.insert(NSWorkspaceDesktopImageScalingKey, scaling_object);
                options.insert(NSWorkspaceDesktopImageAllowClippingKey, clipping_object);
            }
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
pub fn apply(_app: &tauri::AppHandle, path: &Path, _display_mode: &str) -> Result<(), String> {
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
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinuxDesktop {
    Gnome,
    Cinnamon,
    Mate,
    Kde,
    Xfce,
    Unsupported,
}

#[cfg(any(target_os = "linux", test))]
fn desktop_hint_contains(value: &str, hint: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .any(|token| {
            let token = token.to_ascii_lowercase();
            token == hint
                // Session names commonly use these suffixes (for example
                // "plasmawayland" and "xfce4"). Keep the prefix matching
                // narrow so an unrelated value such as "material" does not
                // look like the MATE desktop.
                || matches!(hint, "plasma" | "xfce") && token.starts_with(hint)
        })
}

#[cfg(any(target_os = "linux", test))]
fn detect_linux_desktop(
    current_desktop: &str,
    session_desktop: &str,
    desktop_session: &str,
    kde_full_session: bool,
) -> LinuxDesktop {
    let hints = [current_desktop, session_desktop, desktop_session];
    let contains = |hint: &str| hints.iter().any(|value| desktop_hint_contains(value, hint));

    // Check Plasma and Xfce before the older generic desktop checks. Some
    // distributions expose more than one name in XDG_CURRENT_DESKTOP, for
    // example "KDE;Plasma" or "X-Cinnamon".
    if kde_full_session || contains("kde") || contains("plasma") {
        LinuxDesktop::Kde
    } else if contains("xfce") {
        LinuxDesktop::Xfce
    } else if contains("gnome") || contains("unity") || contains("budgie") {
        LinuxDesktop::Gnome
    } else if contains("cinnamon") {
        LinuxDesktop::Cinnamon
    } else if contains("mate") {
        LinuxDesktop::Mate
    } else {
        LinuxDesktop::Unsupported
    }
}

#[cfg(target_os = "linux")]
fn run_linux_tool(
    command: &mut std::process::Command,
    operation: &str,
) -> Result<std::process::Output, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    match command.output() {
        Ok(output) if output.status.success() => Ok(output),
        Ok(output) => {
            let status = output
                .status
                .code()
                .map(|code| format!(" (exit code {code})"))
                .unwrap_or_else(|| " (terminated by a signal)".into());
            let detail = [
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            ]
            .into_iter()
            .find(|text| !text.is_empty())
            .unwrap_or_default();
            if detail.is_empty() {
                Err(format!("{operation} failed{status} ({program} returned no details)"))
            } else {
                Err(format!("{operation} failed{status}: {detail}"))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "{operation} is unavailable: {program} was not found in PATH"
        )),
        Err(error) => Err(format!("{operation} could not start {program}: {error}")),
    }
}

#[cfg(any(target_os = "linux", test))]
fn kde_fill_mode(display_mode: &str) -> &'static str {
    match display_mode {
        "fit" => "preserveAspectFit",
        "center" => "pad",
        "stretch" => "stretch",
        // Plasma's image wallpaper plugin has no tile mode. Keep the
        // aspect ratio and crop rather than passing an invalid value to the
        // official helper (which would reject the whole update).
        "tile" => "preserveAspectCrop",
        // `fill` is Pinpaper's default: preserve the aspect ratio while
        // filling the display, cropping the excess edges.
        _ => "preserveAspectCrop",
    }
}

#[cfg(any(target_os = "linux", test))]
fn kde_path_arg(path: &Path) -> Result<std::ffi::OsString, String> {
    let path = path
        .to_str()
        .ok_or("KDE Plasma requires a UTF-8 wallpaper path")?;
    // KDE's official helper passes the path into a Plasma JavaScript snippet.
    // It rejects apostrophes itself, but rejecting all characters that would
    // need JavaScript escaping gives the user a useful error and keeps the
    // path a single, literal process argument. No shell is involved here.
    if path
        .chars()
        .any(|character| matches!(character, '\'' | '\\' | '\n' | '\r' | '\0'))
    {
        return Err(
            "KDE Plasma cannot safely use a wallpaper path containing a quote, backslash, or newline"
                .into(),
        );
    }
    Ok(path.into())
}

#[cfg(any(target_os = "linux", test))]
fn kde_command_args(path: &Path, display_mode: &str) -> Result<Vec<std::ffi::OsString>, String> {
    Ok(vec![
        "--fill-mode".into(),
        kde_fill_mode(display_mode).into(),
        kde_path_arg(path)?,
    ])
}

#[cfg(any(target_os = "linux", test))]
fn kde_legacy_command_args(path: &Path) -> Result<Vec<std::ffi::OsString>, String> {
    Ok(vec![kde_path_arg(path)?])
}

#[cfg(any(target_os = "linux", test))]
fn kde_help_supports_fill_mode(stdout: &[u8], stderr: &[u8]) -> bool {
    [stdout, stderr].iter().any(|output| {
        String::from_utf8_lossy(output)
            .split_whitespace()
            .any(|argument| argument == "--fill-mode" || argument == "-f")
    })
}

#[cfg(target_os = "linux")]
fn apply_kde(path: &Path, display_mode: &str) -> Result<(), String> {
    // Plasma 5.27 ships the same helper name but predates its --fill-mode
    // option. Probe the help text so both Plasma generations can set the
    // image without turning a valid wallpaper change into an unknown-option
    // error. The helper itself still reports missing tools and DBus failures.
    let mut help_command = std::process::Command::new("plasma-apply-wallpaperimage");
    let supports_fill_mode = help_command
        .arg("--help")
        .output()
        .map(|output| {
            output.status.success() && kde_help_supports_fill_mode(&output.stdout, &output.stderr)
        })
        .unwrap_or(false);
    let args = if supports_fill_mode {
        kde_command_args(path, display_mode)?
    } else {
        kde_legacy_command_args(path)?
    };
    let mut command = std::process::Command::new("plasma-apply-wallpaperimage");
    command.args(args);
    run_linux_tool(&mut command, "KDE Plasma wallpaper update").map(|_| ())
}

#[cfg(any(target_os = "linux", test))]
fn valid_xfconf_path(path: &str, suffix: &str) -> bool {
    path.starts_with("/backdrop/")
        && path.ends_with(suffix)
        && path.chars().all(|character| !character.is_control())
}

#[cfg(any(target_os = "linux", test))]
fn xfce_property_paths(listing: &str, suffix: &str) -> Vec<String> {
    let mut paths = std::collections::BTreeSet::new();
    for line in listing.lines() {
        let path = line.trim();
        if valid_xfconf_path(path, suffix) {
            paths.insert(path.to_owned());
        }
    }
    paths.into_iter().collect()
}

#[cfg(any(target_os = "linux", test))]
fn xfce_wallpaper_paths(listing: &str) -> Vec<String> {
    let mut paths = xfce_property_paths(listing, "/last-image");
    paths.extend(xfce_property_paths(listing, "/image-path"));
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(any(target_os = "linux", test))]
fn xfce_image_style(display_mode: &str) -> &'static str {
    match display_mode {
        "fit" => "4",     // scaled, preserving the aspect ratio
        "center" => "1",  // centered at the source size
        "tile" => "2",    // repeated in both directions
        "stretch" => "3", // stretched independently on each axis
        _ => "5",          // zoomed/cropped, preserving the aspect ratio
    }
}

#[cfg(any(target_os = "linux", test))]
fn xfconf_set_args(property: &str, value: &std::ffi::OsStr) -> Vec<std::ffi::OsString> {
    vec![
        "-c".into(),
        "xfce4-desktop".into(),
        "-p".into(),
        property.into(),
        "-s".into(),
        value.to_owned(),
    ]
}

#[cfg(target_os = "linux")]
fn xfconf_set(property: &str, value: &std::ffi::OsStr) -> Result<(), String> {
    let args = xfconf_set_args(property, value);
    let mut command = std::process::Command::new("xfconf-query");
    command.args(args);
    run_linux_tool(
        &mut command,
        &format!("Xfce wallpaper property update for {property}"),
    )
    .map(|_| ())
}

#[cfg(target_os = "linux")]
fn apply_xfce(path: &Path, display_mode: &str) -> Result<(), String> {
    let path = path
        .to_str()
        .ok_or("Xfce requires a UTF-8 wallpaper path")?;
    let mut listing_command = std::process::Command::new("xfconf-query");
    listing_command.args(["-c", "xfce4-desktop", "-l"]);
    let listing = run_linux_tool(&mut listing_command, "Xfce wallpaper configuration query")?;
    let listing = String::from_utf8_lossy(&listing.stdout);
    let wallpaper_paths = xfce_wallpaper_paths(&listing);
    if wallpaper_paths.is_empty() {
        return Err(
            "Xfce did not expose any configured wallpaper monitor/workspace properties; open Xfce's desktop settings once, then try again"
                .into(),
        );
    }

    // Xfce stores settings per monitor and, on current versions, per
    // workspace. Updating every discovered path keeps all monitors and
    // workspaces in sync, including monitor names such as monitorDP-1.
    for property in wallpaper_paths {
        xfconf_set(&property, std::ffi::OsStr::new(path))?;
    }
    for property in xfce_property_paths(&listing, "/image-style") {
        xfconf_set(
            &property,
            std::ffi::OsStr::new(xfce_image_style(display_mode)),
        )?;
    }
    // A user may have previously selected a solid-color backdrop. When an
    // image is explicitly selected in Pinpaper, turn the corresponding image
    // switches back on if the desktop exposes them.
    for property in xfce_property_paths(&listing, "/image-show") {
        xfconf_set(&property, std::ffi::OsStr::new("true"))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn apply(_app: &tauri::AppHandle, path: &Path, display_mode: &str) -> Result<(), String> {
    let current_desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session_desktop = std::env::var("XDG_SESSION_DESKTOP").unwrap_or_default();
    let desktop_session = std::env::var("DESKTOP_SESSION").unwrap_or_default();
    let kde_full_session = std::env::var("KDE_FULL_SESSION")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("KDE_SESSION_VERSION")
            .map(|value| !value.is_empty())
            .unwrap_or(false);
    let desktop = detect_linux_desktop(
        &current_desktop,
        &session_desktop,
        &desktop_session,
        kde_full_session,
    );
    let uri = url::Url::from_file_path(path)
        .map_err(|_| "Invalid wallpaper path")?
        .to_string();
    let run = |schema: &str, key: &str, value: &str| -> Result<(), String> {
        let mut command = std::process::Command::new("gsettings");
        command.args(["set", schema, key, value]);
        run_linux_tool(&mut command, "Linux wallpaper update").map(|_| ())
    };
    match desktop {
        LinuxDesktop::Kde => apply_kde(path, display_mode),
        LinuxDesktop::Xfce => apply_xfce(path, display_mode),
        LinuxDesktop::Gnome => {
            run("org.gnome.desktop.background", "picture-uri", &uri)?;
            run("org.gnome.desktop.background", "picture-uri-dark", &uri)
        }
        LinuxDesktop::Cinnamon => run("org.cinnamon.desktop.background", "picture-uri", &uri),
        LinuxDesktop::Mate => run(
            "org.mate.background",
            "picture-filename",
            path.to_str().ok_or("Non UTF-8 path")?,
        ),
        LinuxDesktop::Unsupported => Err(
            "This Linux desktop is not supported. Pinpaper supports GNOME, Unity, Budgie, Cinnamon, MATE, KDE Plasma, and Xfce; wlroots-only desktops are not supported."
                .into(),
        ),
    }
}

#[cfg(test)]
mod download_tests {
    use super::*;
    #[test]
    fn old_decoded_thumbnail_cache_requires_metadata_recovery() {
        let dir = std::env::temp_dir().join(format!("pinpaper-thumbnail-regression-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut pin = Pin { id: "000000042424242".into(),
            url: "https://i.pinimg.com/236x/regression.jpg".into(),
            width: 236, height: 157, dimensions_verified: true,
            dimensions_source: crate::model::DimensionSource::Decoded,
            dimensions_url: Some("https://i.pinimg.com/236x/regression.jpg".into()),
            ..Default::default() };
        let path = quality_cache(&dir, &pin);
        image::RgbImage::new(236, 157).save(&path).unwrap();
        assert!(network::needs_pin_resolution(&pin));
        assert!(!cached(&dir, &pin).exists());
        // A confirmed original uses a separate existing cache key, without
        // deleting the user's old fallback files.
        pin.url = "https://i.pinimg.com/originals/regression.png".into();
        pin.original_url_exact = true;
        assert!(!network::needs_pin_resolution(&pin));
        assert_ne!(quality_cache(&dir, &pin), path);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "explicit live Pinterest recovery; writes only to a temporary folder"]
    fn live_pinterest_thumbnail_recovery() {
        let id = std::env::var("PINPAPER_LIVE_PIN_ID").expect("set PINPAPER_LIVE_PIN_ID");
        let thumbnail = std::env::var("PINPAPER_LIVE_THUMBNAIL").expect("set PINPAPER_LIVE_THUMBNAIL");
        let source = network::resolve_pin_image(&id).unwrap();
        assert!(source.primary_is_original, "page did not expose original: {:?}", source);
        assert!(network::same_asset(&thumbnail, &source.primary));
        let pin = Pin { id: id.clone(), url: source.primary,
            original_url_exact: source.primary_is_original, fallback_url: source.fallback,
            source_url: source.source_url, ..Default::default() };
        let dir = std::env::temp_dir().join(format!("pinpaper-live-recovery-{id}"));
        fs::create_dir_all(&dir).unwrap();
        let result = download_with_guard(&dir, &pin, || true).unwrap();
        let (width, height) = decoded_dimensions(&result.path).unwrap();
        println!("pin={id} decoded={width}x{height} source={} path={}", result.source_url, result.path.display());
        assert!(width > 236, "download remained a thumbnail");
        assert!(result.source_url.contains("/originals/") || network::valid_source_url(&result.source_url));
    }

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
    fn decoded_quality_prefers_the_larger_external_or_pinterest_variant() {
        let low = image::DynamicImage::ImageRgb8(image::RgbImage::new(736, 414));
        let high = image::DynamicImage::ImageRgb8(image::RgbImage::new(2560, 1440));
        assert!(decoded_image_is_better(&high, &low));
        assert!(!decoded_image_is_better(&low, &high));
    }

    #[test]
    fn cached_dimensions_use_the_decoded_file_over_page_metadata() {
        let dir =
            std::env::temp_dir().join(format!("pinpaper-dimensions-{}", rand::random::<u64>()));
        fs::create_dir_all(&dir).unwrap();
        let pin = Pin {
            id: "123".into(),
            board_id: crate::browser_session::SOURCE.into(),
            url: "https://i.pinimg.com/originals/123.jpg".into(),
            // Simulate the overstated Pinterest metadata from the reported
            // bug. The file on disk is the authority after download.
            max_width: 4000,
            max_height: 3000,
            ..Default::default()
        };
        image::RgbImage::new(640, 480)
            .save(quality_cache(&dir, &pin))
            .unwrap();

        assert_eq!(cached_dimensions(&dir, &pin), Some((640, 480)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_fallback_cache_is_reused_only_during_negative_source_backoff() {
        let dir =
            std::env::temp_dir().join(format!("pinpaper-source-cache-{}", rand::random::<u64>()));
        fs::create_dir_all(&dir).unwrap();
        let pin = Pin {
            id: "source-cache".into(),
            board_id: crate::browser_session::SOURCE.into(),
            url: "https://i.pinimg.com/736x/source-cache.jpg".into(),
            source_url: Some(format!(
                "https://wallhaven.cc/w/zpvp3g-cache-{}",
                rand::random::<u64>()
            )),
            dimensions_url: Some("https://i.pinimg.com/736x/source-cache.jpg".into()),
            ..Default::default()
        };
        let path = quality_cache(&dir, &pin);
        fs::write(&path, b"fallback").unwrap();
        assert_eq!(cached(&dir, &pin), path.with_extension("retry"));
        network::mark_external_source_unavailable(&pin);
        assert_eq!(cached(&dir, &pin), path);
        fs::remove_dir_all(dir).unwrap();
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
    fn foreground_search_checks_all_saved_candidates_when_downloads_fail() {
        let dir =
            std::env::temp_dir().join(format!("pinpaper-foreground-{}", rand::random::<u64>()));
        fs::create_dir_all(&dir).unwrap();
        let pins = (0..12)
            .map(|index| Pin {
                dimensions_verified: false,
                id: index.to_string(),
                board_id: crate::browser_session::SOURCE.into(),
                title: format!("Pin {index}"),
                description: String::new(),
                url: format!("https://i.pinimg.com/236x/{index}.jpg"),
                fallback_url: None,
                width: 0,
                height: 0,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let candidates = foreground_candidates(pins, &dir, true);
        let mut attempts = Vec::new();
        let failure = select_foreground(
            candidates,
            |_| false,
            |pin| (crate::model::image_identity(pin), pin.url.clone()),
            |attempt, pin| attempts.push((attempt, pin.id.clone())),
            |_, _, _| {},
            |_| Err::<(), _>("download failed".into()),
        )
        .unwrap_err();
        assert_eq!(
            attempts,
            (1..=12)
                .map(|attempt| (attempt, (attempt - 1).to_string()))
                .collect::<Vec<_>>()
        );
        assert_eq!(failure.attempts, 12);
        assert_eq!(failure.max_attempts, 12);
        assert!(!failure.exhausted_budget);
        assert_eq!(failure.candidates, 12);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn foreground_search_crosses_a_two_pin_round_tail_without_reordering_cached_history() {
        let dir =
            std::env::temp_dir().join(format!("pinpaper-round-tail-{}", rand::random::<u64>()));
        fs::create_dir_all(&dir).unwrap();
        let mut library = crate::model::Library::default();
        library.settings.board_ids = vec!["board".into()];
        library.pins = (0..10)
            .map(|index| Pin {
                dimensions_verified: true,
                id: index.to_string(),
                board_id: "board".into(),
                title: format!("Pin {index}"),
                description: String::new(),
                url: format!("https://i.pinimg.com/736x/{index}.jpg"),
                fallback_url: None,
                width: 1920,
                height: 1080,
                ..Default::default()
            })
            .collect();
        for index in 0..8 {
            let used = library.pins[index].clone();
            crate::model::record_rotation(&mut library, &used);
        }
        library.history = (0..40).map(|index| format!("old-{index}")).collect();
        // The current wallpaper is excluded by the normal ranking filter, so
        // the persisted pool has two unseen pins and seven usable next-round
        // fallbacks.
        library.current = Some(library.pins[0].clone());
        for pin in library.pins.iter().skip(1).take(7) {
            fs::write(quality_cache(&dir, pin), b"cached").unwrap();
        }

        // Exercise the same persisted state shape loaded by Engine::setup.
        let library: crate::model::Library =
            serde_json::from_slice(&serde_json::to_vec(&library).unwrap()).unwrap();
        let candidates = crate::model::foreground_rotation_candidates(&library);
        failures()
            .lock()
            .unwrap()
            .insert(candidates[2].url.clone(), Instant::now());
        // This mirrors Engine::next(true): a manual click clears background
        // cooldowns before building the foreground pool.
        clear_temporary_unavailable(candidates.iter());
        let candidates = foreground_candidates(candidates, &dir, true);
        let mut attempts = Vec::new();
        let failure = select_foreground(
            candidates,
            |pin| cached(&dir, pin).exists(),
            |pin| (crate::model::image_identity(pin), pin.url.clone()),
            |attempt, pin| attempts.push((attempt, pin.id.clone())),
            |_, _, _| {},
            |_| Err::<(), _>("Image download failed".into()),
        )
        .unwrap_err();

        assert_eq!(attempts.len(), 9);
        assert_eq!(attempts.first().map(|(_, id)| id.as_str()), Some("8"));
        assert_eq!(attempts.last().map(|(_, id)| id.as_str()), Some("7"));
        assert_eq!(failure.attempts, 9);
        assert_eq!(failure.max_attempts, 9);
        assert!(!failure.exhausted_budget);
        assert_eq!(failure.candidates, 9);
        failures().lock().unwrap().clear();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persisted_rotation_cursor_moves_past_three_cached_successes_in_a_428_pin_library() {
        let mut library = crate::model::Library::default();
        library.settings.board_ids = vec!["board".into()];
        library.pins = (0..428)
            .map(|index| Pin {
                dimensions_verified: true,
                id: format!("{index:03}"),
                board_id: "board".into(),
                title: format!("Landscape pin {index}"),
                description: String::new(),
                url: format!("https://i.pinimg.com/736x/{index:03}.jpg"),
                fallback_url: None,
                width: 1920,
                height: 1080,
                ..Default::default()
            })
            .collect();

        // Three previously cached images have already completed the round.
        for index in 0..3 {
            let pin = library.pins[index].clone();
            crate::model::record_rotation(&mut library, &pin);
        }
        library.current = Some(library.pins[2].clone());

        // Exercise the same persisted state shape loaded during application
        // setup, then fail every remaining URL exactly as a foreground request
        // would after a temporary CDN refusal.
        let mut library: crate::model::Library =
            serde_json::from_slice(&serde_json::to_vec(&library).unwrap()).unwrap();
        crate::model::begin_rotation_round(&mut library);
        let first_batch = crate::model::foreground_rotation_candidates(&library);
        assert_eq!(first_batch.len(), 427);
        assert_eq!(first_batch[0].id, "003");
        let mut rejected = Vec::new();
        let failure = select_foreground(
            first_batch,
            |_| false,
            |pin| (crate::model::image_identity(pin), pin.url.clone()),
            |_, _| {},
            |_, pin, _| rejected.push(pin.clone()),
            |pin| Err::<(), _>(format!("temporary download failure for {}", pin.id)),
        )
        .unwrap_err();
        assert_eq!(failure.attempts, 427);
        assert_eq!(failure.max_attempts, 427);
        assert!(!failure.exhausted_budget);
        assert_eq!(rejected.len(), 427);
        assert_eq!(rejected.first().map(|pin| pin.id.as_str()), Some("003"));
        assert_eq!(rejected.last().map(|pin| pin.id.as_str()), Some("001"));
        for pin in rejected {
            crate::model::record_rotation_attempt(&mut library, &pin);
        }

        // A restart sees a completed saved-picture round and starts the next
        // round at the first eligible image instead of applying an arbitrary
        // eight-attempt cutoff.
        let mut library: crate::model::Library =
            serde_json::from_slice(&serde_json::to_vec(&library).unwrap()).unwrap();
        assert!(crate::model::begin_rotation_round(&mut library));
        let next_batch = crate::model::foreground_rotation_candidates(&library);
        assert_eq!(next_batch[0].id, "000");
        let mut attempts = Vec::new();
        let selected = select_foreground(
            next_batch,
            |_| false,
            |pin| (crate::model::image_identity(pin), pin.url.clone()),
            |_, pin| attempts.push(pin.id.clone()),
            |_, _, _| {},
            |pin| {
                if pin.id == "000" {
                    Ok(pin.id.clone())
                } else {
                    Err("unexpected repeat before the unattempted tail".into())
                }
            },
        )
        .unwrap();
        assert_eq!(selected, "000");
        assert_eq!(attempts, vec!["000"]);
    }

    #[test]
    fn foreground_search_reports_user_cancellation_without_marking_a_failure() {
        let mut attempts = Vec::new();
        let failure = select_foreground(
            0..100,
            |_| false,
            |pin| (pin.to_string(), pin.to_string()),
            |attempt, pin| attempts.push((attempt, *pin)),
            |_, _, _| panic!("cancellation must not be reported as an image failure"),
            |_| Err::<(), _>(CHANGE_CANCELLED.into()),
        )
        .unwrap_err();

        assert!(failure.cancelled);
        assert_eq!(attempts, vec![(1, 0)]);
        assert_eq!(failure.candidates, 100);
        assert_eq!(failure.max_attempts, 100);
        assert!(!failure.exhausted_budget);
    }

    #[test]
    fn foreground_search_counts_successes_separately_from_attempts() {
        let mut attempts = Vec::new();
        let selected = select_foreground_with_target(
            0..5,
            2,
            |_| false,
            |pin| (pin.to_string(), pin.to_string()),
            |attempt, pin| attempts.push((attempt, *pin)),
            |_, _, _| {},
            |pin| {
                if pin == 0 {
                    Err("temporary download failure".into())
                } else {
                    Ok(pin)
                }
            },
        )
        .unwrap();

        assert_eq!(selected, vec![1, 2]);
        assert_eq!(attempts, vec![(1, 0), (2, 1), (3, 2)]);
    }

    #[test]
    fn foreground_search_reports_source_exhaustion_before_success_target() {
        let failure = select_foreground_with_target(
            0..3,
            2,
            |_| false,
            |pin| (pin.to_string(), pin.to_string()),
            |_, _| {},
            |_, _, _| {},
            |_| Err::<(), _>("download failed".into()),
        )
        .unwrap_err();

        assert_eq!(failure.attempts, 3);
        assert_eq!(failure.candidates, 3);
        assert_eq!(failure.successful, 0);
        assert_eq!(failure.target_successes, 2);
        assert!(failure.source_exhausted);
        assert!(!failure.exhausted_budget);
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
                        Ok((path.clone(), "https://i.pinimg.com/736x/cache.jpg".into()))
                    })
                    .unwrap();
                    assert_eq!(fs::read(path).unwrap(), b"complete image");
                });
            }
        });
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        let (_, source) = cache_once(&path, || Err("cache should already exist".into())).unwrap();
        assert_eq!(source, "https://i.pinimg.com/736x/cache.jpg");
        let retry = dir.join("retry.jpg");
        assert!(cache_once(&retry, || Err("temporary failure".into())).is_err());
        cache_once(&retry, || {
            fs::write(&retry, b"retried").unwrap();
            Ok((retry.clone(), String::new()))
        })
        .unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reset_cache_invalidates_old_generation_and_removes_ephemeral_cache() {
        use std::sync::atomic::AtomicU64;
        let dir = std::env::temp_dir().join(format!(
            "pinpaper-reset-cache-test-{}",
            rand::random::<u64>()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("wallpaper.png"), b"cached image").unwrap();
        fs::write(dir.join("wallpaper.preview.jpg"), b"preview").unwrap();
        let generation = AtomicU64::new(41);

        reset_cache(&dir, &generation).unwrap();

        assert!(!dir.exists());
        assert_eq!(generation.load(Ordering::SeqCst), 42);
    }

    #[test]
    fn foreground_retry_clears_only_matching_failure_cooldowns() {
        let matching = Pin {
            dimensions_verified: false,
            id: "matching".into(),
            board_id: "board".into(),
            title: String::new(),
            description: String::new(),
            url: "https://i.pinimg.com/236x/matching.jpg".into(),
            fallback_url: None,
            width: 0,
            height: 0,
            ..Default::default()
        };
        let unrelated = Pin {
            id: "unrelated".into(),
            url: "https://i.pinimg.com/236x/unrelated.jpg".into(),
            ..matching.clone()
        };
        {
            let mut failed = failures().lock().unwrap();
            failed.insert(matching.url.clone(), Instant::now());
            failed.insert(unrelated.url.clone(), Instant::now());
        }
        assert!(temporarily_unavailable(&matching));
        assert!(temporarily_unavailable(&unrelated));
        clear_temporary_unavailable(std::iter::once(&matching));
        assert!(!temporarily_unavailable(&matching));
        assert!(temporarily_unavailable(&unrelated));
        failures().lock().unwrap().clear();
    }
}

#[cfg(test)]
mod linux_backend_tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn detects_common_kde_and_xfce_session_names() {
        assert_eq!(
            detect_linux_desktop("KDE;Plasma", "", "", false),
            LinuxDesktop::Kde
        );
        assert_eq!(
            detect_linux_desktop("", "plasmawayland", "", false),
            LinuxDesktop::Kde
        );
        assert_eq!(
            detect_linux_desktop("X-XFCE", "", "", false),
            LinuxDesktop::Xfce
        );
        assert_eq!(
            detect_linux_desktop("", "", "xfce", false),
            LinuxDesktop::Xfce
        );
        assert_eq!(
            detect_linux_desktop("X-Cinnamon", "", "", false),
            LinuxDesktop::Cinnamon
        );
        assert_eq!(
            detect_linux_desktop("", "", "", true),
            LinuxDesktop::Kde
        );
        assert_eq!(
            detect_linux_desktop("sway", "", "", false),
            LinuxDesktop::Unsupported
        );
        assert_eq!(
            detect_linux_desktop("material-shell", "", "", false),
            LinuxDesktop::Unsupported
        );
    }

    #[test]
    fn maps_pinpaper_display_modes_to_upstream_tools() {
        assert_eq!(kde_fill_mode("fill"), "preserveAspectCrop");
        assert_eq!(kde_fill_mode("fit"), "preserveAspectFit");
        assert_eq!(kde_fill_mode("center"), "pad");
        assert_eq!(kde_fill_mode("tile"), "preserveAspectCrop");
        assert_eq!(kde_fill_mode("stretch"), "stretch");
        assert_eq!(xfce_image_style("fill"), "5");
        assert_eq!(xfce_image_style("fit"), "4");
        assert_eq!(xfce_image_style("center"), "1");
        assert_eq!(xfce_image_style("tile"), "2");
        assert_eq!(xfce_image_style("stretch"), "3");
    }

    #[test]
    fn discovers_all_xfce_monitor_and_workspace_properties() {
        let listing = "\
/backdrop/screen0/monitorDP-1/workspace0/last-image
/backdrop/screen0/monitorDP-1/workspace1/last-image
/backdrop/screen0/monitorHDMI-1/workspace0/image-path
/backdrop/screen0/monitorHDMI-1/workspace0/image-style
/backdrop/screen0/monitorHDMI-1/workspace0/image-show
/desktop-icons/style
/not-a-backdrop/monitor0/last-image
";
        assert_eq!(
            xfce_wallpaper_paths(listing),
            vec![
                "/backdrop/screen0/monitorDP-1/workspace0/last-image".to_owned(),
                "/backdrop/screen0/monitorDP-1/workspace1/last-image".to_owned(),
                "/backdrop/screen0/monitorHDMI-1/workspace0/image-path".to_owned(),
            ]
        );
        assert_eq!(
            xfce_property_paths(listing, "/image-style"),
            vec!["/backdrop/screen0/monitorHDMI-1/workspace0/image-style".to_owned()]
        );
    }

    #[test]
    fn process_arguments_keep_shell_metacharacters_as_literal_values() {
        let path = Path::new("/tmp/wallpapers/photo with spaces; $HOME & \"quoted\".png");
        let kde_args = kde_command_args(path, "fill").unwrap();
        assert_eq!(kde_args[0], OsString::from("--fill-mode"));
        assert_eq!(kde_args[1], OsString::from("preserveAspectCrop"));
        assert_eq!(kde_args[2], path.as_os_str().to_owned());
        assert_eq!(
            kde_legacy_command_args(path).unwrap(),
            vec![path.as_os_str().to_owned()]
        );
        assert!(kde_help_supports_fill_mode(
            b"Options: -f, --fill-mode <fill-mode>",
            b""
        ));
        assert!(!kde_help_supports_fill_mode(
            b"Options: --help, --version",
            b""
        ));

        let xfce_args = xfconf_set_args(
            "/backdrop/screen0/monitorDP-1/workspace0/last-image",
            OsStr::new("/tmp/a wallpaper; printf unsafe 'text'.png"),
        );
        assert_eq!(xfce_args[0], OsString::from("-c"));
        assert_eq!(
            xfce_args[3],
            OsString::from("/backdrop/screen0/monitorDP-1/workspace0/last-image")
        );
        assert_eq!(
            xfce_args[5],
            OsString::from("/tmp/a wallpaper; printf unsafe 'text'.png")
        );
        assert!(kde_command_args(Path::new("/tmp/a'quote.png"), "fill").is_err());
    }
}
