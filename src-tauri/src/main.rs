#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod autostart;
mod browser_session;
mod language;
mod model;
mod network;
mod prefetch;
mod preview;
mod wallpaper;
use chrono::Timelike;
use model::{Library, Settings};
use serde::Serialize;
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager,
};
struct Engine {
    app: tauri::AppHandle,
    library: Mutex<Library>,
    error: Mutex<Option<String>>,
    busy: AtomicBool,
    changing: AtomicBool,
    cancel_change: AtomicBool,
    change_status: Mutex<Option<ChangeStatus>>,
    warm: std::sync::mpsc::SyncSender<()>,
    browser_connected: AtomicBool,
    bridge: browser_session::ImportBridge,
    data: PathBuf,
    cache: PathBuf,
    cache_generation: AtomicU64,
    preview: Mutex<preview::PreviewCache>,
}
#[derive(Clone, Serialize)]
struct ChangeStatus {
    phase: &'static str,
    attempt: usize,
    max_attempts: usize,
}
#[derive(Serialize)]
struct Snapshot {
    library: Library,
    total_pictures: usize,
    selected_pictures: usize,
    available_pictures: usize,
    unknown_resolution_pictures: usize,
    hidden_pictures: usize,
    connected: bool,
    browser_connected: bool,
    browser_open: bool,
    busy: bool,
    changing: bool,
    change_status: Option<ChangeStatus>,
    error: Option<String>,
    preview: Option<String>,
    locale: String,
}
impl Engine {
    fn set_change_status(&self, status: Option<ChangeStatus>) {
        *self.change_status.lock().unwrap() = status;
        let _ = self.app.emit_to("main", "pinpaper-changed", ());
    }

    fn save(&self, lib: &Library) -> Result<(), String> {
        fs::create_dir_all(&self.data).map_err(|_| "Cannot create data directory")?;
        let temp = self.data.join("library.tmp");
        let target = self.data.join("library.json");
        fs::write(
            &temp,
            serde_json::to_vec(lib).map_err(|_| "Cannot encode library")?,
        )
        .map_err(|_| "Cannot save library")?;
        // Windows rename does not replace existing files.
        #[cfg(target_os = "windows")]
        if target.exists() {
            fs::copy(&temp, &target).map_err(|_| "Cannot save library")?;
            fs::remove_file(&temp).ok();
            return Ok(());
        }
        fs::rename(temp, target).map_err(|_| "Cannot save library".into())
    }
    fn prepare_pin(&self, pin: &model::Pin) -> Result<model::Pin, String> {
        if !network::needs_pin_resolution(pin) { return Ok(pin.clone()); }
        let generation = self.cache_generation.load(Ordering::SeqCst);
        let source = match network::resolve_pin_image(&pin.id) {
            Ok(source) if network::same_asset(&pin.url, &source.primary) => source,
            // A missing/private/deleted page must not replace the pin with
            // a related recommendation. Retain the observed image as fallback.
            _ => return Ok(pin.clone()),
        };
        let mut updated_pin = pin.clone();
        let source_identity = if let Some(asset) = network::asset_identity(&source.primary) {
            format!("url:{asset}")
        } else {
            format!("pin:{}:{}", pin.board_id, pin.id)
        };
        let same_asset = model::image_identity(pin) == source_identity;
        // A generic social preview may be smaller than the feed variant.
        // Only promote it when the page supplied an original or a larger size.
        let cdn_width = |url: &str| url::Url::parse(url).ok().and_then(|url| {
            url.path_segments()?.next()?.split_once('x')?.0.parse::<u32>().ok()
        }).unwrap_or(0);
        if !source.primary_is_original && cdn_width(&source.primary) < cdn_width(&pin.url) {
            return Ok(pin.clone());
        }
        updated_pin.url = source.primary.clone();
        if source.source_url.is_some() {
            updated_pin.source_url = source.source_url.clone();
        }
        updated_pin.fallback_url = source.fallback.clone();
        updated_pin.original_url_exact = source.primary_is_original;
        // Page metadata is not a decoded image. Re-verify dimensions after the
        // observed URL has actually been downloaded.
        updated_pin.dimensions_verified = false;
        updated_pin.width = 0;
        updated_pin.height = 0;
        updated_pin.dimensions_source = model::DimensionSource::Unknown;
        updated_pin.dimensions_url = None;
        if !same_asset {
            updated_pin.max_width = 0;
            updated_pin.max_height = 0;
            updated_pin.max_dimensions_source = model::DimensionSource::Unknown;
            updated_pin.max_dimensions_url = None;
            updated_pin.thumbnail_width = 0;
            updated_pin.thumbnail_height = 0;
        }
        if source.max_width > 0 && source.max_height > 0 {
            updated_pin.max_width = source.max_width;
            updated_pin.max_height = source.max_height;
            updated_pin.max_dimensions_source = model::DimensionSource::PinterestOriginal;
            updated_pin.max_dimensions_url = source.max_dimensions_url.clone();
        }
        if source.thumbnail_width > 0 && source.thumbnail_height > 0 {
            updated_pin.thumbnail_width = source.thumbnail_width;
            updated_pin.thumbnail_height = source.thumbnail_height;
        }
        let mut library = self.library.lock().unwrap();
        if self.cache_generation.load(Ordering::SeqCst) != generation {
            return Err("Image download cancelled by local-data reset".into());
        }
        if let Some(stored) = library
            .pins
            .iter_mut()
            .find(|stored| stored.id == pin.id && stored.board_id == pin.board_id)
        {
            *stored = updated_pin.clone();
            self.save(&library)?;
        }
        Ok(updated_pin)
    }

    fn next(&self, manual_retry: bool) -> Result<(), String> {
        struct Changing<'a>(&'a Engine);
        impl Drop for Changing<'_> {
            fn drop(&mut self) {
                self.0.changing.store(false, Ordering::SeqCst);
                self.0.set_change_status(None);
                let _ = self.0.app.emit_to("main", "pinpaper-changed", ());
            }
        }
        // A stop request belongs to one foreground change. Clear the previous
        // request only when a new change actually starts.
        self.cancel_change.store(false, Ordering::SeqCst);
        self.changing.store(true, Ordering::SeqCst);
        let _changing = Changing(self);
        let _ = self.app.emit_to("main", "pinpaper-changed", ());
        let candidates = {
            let mut lib = self.library.lock().unwrap();
            let dimensions_changed = model::canonicalize_dimensions(&mut lib)
                | refresh_cached_dimensions(&self.cache, &mut lib);
            if dimensions_changed {
                self.save(&lib)?;
            }
            // A new round is allowed only after every currently eligible pin
            // has been inspected. Failed candidates remain separate from
            // successful history, so a few cached successes cannot become an
            // endless fallback while the rest of the collection is pending.
            model::begin_rotation_round(&mut lib);
            model::foreground_rotation_candidates(&lib)
        };
        if candidates.is_empty() {
            return Err("No matching pictures. Choose a collection in Pictures to use, add pictures, or relax your picture preferences.".into());
        }
        if self.cancel_change.load(Ordering::SeqCst) {
            return Err(wallpaper::CHANGE_CANCELLED.into());
        }
        if manual_retry {
            // A deliberate click is an explicit request to retry matching
            // images now. Scheduled changes retain the five-minute cooldown.
            wallpaper::clear_temporary_unavailable(candidates.iter());
        }
        let list = wallpaper::foreground_candidates(candidates, &self.cache, manual_retry);
        if list.is_empty() {
            return Err("Wallpaper search paused because matching pictures are temporarily unavailable after previous download failures. Retry the wallpaper search now; if the links keep failing, re-import the Pinterest pictures.".into());
        }
        // The success target is one applied wallpaper for this command. The
        // retry/progress limit is the finite set of selected saved pictures;
        // failures and duplicates do not reduce the success target, and the
        // selector walks the remaining set without the old fixed limit of
        // eight attempts.
        let candidate_limit = list.len();
        let target_successes = wallpaper::FOREGROUND_TARGET_SUCCESSES;
        let settings = self.library.lock().unwrap().settings.clone();
        let mut rejected_attempts = Vec::new();
        let selected = wallpaper::select_foreground_with_target(
            list,
            target_successes,
            |pin| wallpaper::cached(&self.cache, pin).exists(),
            |pin| (model::image_identity(pin), pin.url.clone()),
            |attempt, _| {
                self.set_change_status(Some(ChangeStatus {
                    phase: if attempt == 1 {
                        "searching"
                    } else {
                        "retrying"
                    },
                    attempt,
                    max_attempts: candidate_limit,
                }));
            },
            |attempt, pin, error| {
                // A downloaded image that fails the current filters is marked
                // verified and disappears from the current eligible set. Do
                // not persist that failed key, so relaxing the filters can
                // make it available again without resetting the whole round.
                if !error.contains("resolution/orientation filters") {
                    rejected_attempts.push(pin.clone());
                }
                self.set_change_status(Some(ChangeStatus {
                    phase: "retrying",
                    attempt,
                    max_attempts: candidate_limit,
                }));
            },
            |pin| {
                if self.cancel_change.load(Ordering::SeqCst) {
                    return Err(wallpaper::CHANGE_CANCELLED.into());
                }
                let mut pin = self.prepare_pin(&pin)?;
                if self.cancel_change.load(Ordering::SeqCst) {
                    return Err(wallpaper::CHANGE_CANCELLED.into());
                }
                let existing = wallpaper::cached(&self.cache, &pin);
                let (path, dimensions_url) = if existing.exists() {
                    (
                        existing,
                        pin.dimensions_url
                            .clone()
                            .unwrap_or_else(|| pin.url.clone()),
                    )
                } else {
                    let expected_generation = self.cache_generation.load(Ordering::SeqCst);
                    let downloaded = wallpaper::download_with_guard(&self.cache, &pin, || {
                        self.cache_generation.load(Ordering::SeqCst) == expected_generation
                            && !self.cancel_change.load(Ordering::SeqCst)
                    })?;
                    if self.cancel_change.load(Ordering::SeqCst) {
                        return Err(wallpaper::CHANGE_CANCELLED.into());
                    }
                    let source_url = if downloaded.source_url.is_empty() {
                        self.library
                            .lock()
                            .unwrap()
                            .pins
                            .iter()
                            .find(|stored| stored.id == pin.id && stored.board_id == pin.board_id)
                            .and_then(|stored| stored.dimensions_url.clone())
                            .filter(|url| !url.is_empty())
                            .unwrap_or_else(|| pin.url.clone())
                    } else {
                        downloaded.source_url
                    };
                    (downloaded.path, source_url)
                };
                pin.dimensions_url = Some(dimensions_url.clone());
                let (w, h) = match wallpaper::decoded_dimensions(&path) {
                    Ok(size) => size,
                    Err(_) => {
                        // Only remove this corrupt file, so a future attempt can fetch it again.
                        let _ = fs::remove_file(&path);
                        return Err("Cached image could not be decoded".into());
                    }
                };
                if self.cancel_change.load(Ordering::SeqCst) {
                    return Err(wallpaper::CHANGE_CANCELLED.into());
                }
                {
                    let mut lib = self.library.lock().unwrap();
                    for candidate in lib
                        .pins
                        .iter_mut()
                        .filter(|p| p.id == pin.id && p.board_id == pin.board_id)
                    {
                        candidate.dimensions_verified = true;
                        candidate.width = w;
                        candidate.height = h;
                        candidate.dimensions_source = model::DimensionSource::Decoded;
                        candidate.dimensions_url = Some(dimensions_url.clone());
                    }
                }
                if w < settings.min_width
                    || (settings.orientation == "landscape" && w <= h)
                    || (settings.orientation == "portrait" && h <= w)
                {
                    return Err(
                        "Downloaded images do not meet your resolution/orientation filters".into(),
                    );
                }
                Ok((pin, path, w, h))
            },
        );
        let (mut pin, path, w, h) = match selected {
            Ok(mut candidates) => candidates
                .pop()
                .expect("foreground success target must produce one candidate"),
            Err(failure) => {
                if failure.cancelled || self.cancel_change.load(Ordering::SeqCst) {
                    return Err(wallpaper::CHANGE_CANCELLED.into());
                }
                // Persist rejected dimensions and the attempted rotation
                // cursor. This is what moves the next click past transiently
                // unavailable candidates instead of restarting at the same
                // first few URLs.
                let mut lib = self.library.lock().unwrap();
                for attempted in &rejected_attempts {
                    model::record_rotation_attempt(&mut lib, attempted);
                }
                self.save(&lib)?;
                return Err(selection_error(failure));
            }
        };
        self.set_change_status(Some(ChangeStatus {
            phase: "applying",
            attempt: 1,
            max_attempts: candidate_limit,
        }));
        if self.cancel_change.load(Ordering::SeqCst) {
            return Err(wallpaper::CHANGE_CANCELLED.into());
        }
        // OS adapter failures are not image failures: stop instead of downloading the library.
        wallpaper::apply(&self.app, &path, &settings.display_mode)?;
        self.preview.lock().unwrap().clear();
        let mut lib = self.library.lock().unwrap();
        for attempted in &rejected_attempts {
            model::record_rotation_attempt(&mut lib, attempted);
        }
        pin.dimensions_verified = true;
        pin.width = w;
        pin.height = h;
        pin.dimensions_source = model::DimensionSource::Decoded;
        lib.current = Some(pin.clone());
        lib.last_change = chrono::Utc::now().timestamp();
        model::record_rotation(&mut lib, &pin);
        lib.history.push(pin.id);
        if lib.history.len() > 30 {
            lib.history.remove(0);
        }
        self.save(&lib)?;
        Ok(())
    }
}

fn refresh_cached_dimensions(cache: &std::path::Path, library: &mut Library) -> bool {
    let mut changed = false;
    for pin in &mut library.pins {
        // A verified decoded pair is already tied to the immutable cache key
        // and needs no header read on every UI snapshot or wallpaper change.
        // Only migrate records whose dimensions are still unknown or whose
        // provenance has not been marked as decoded.
        if pin.dimensions_verified
            && pin.width > 0
            && pin.height > 0
            && pin.dimensions_source == model::DimensionSource::Decoded
        {
            continue;
        }
        if let Some((width, height)) = wallpaper::cached_dimensions(cache, pin) {
            if !pin.dimensions_verified
                || pin.width != width
                || pin.height != height
                || pin.dimensions_source != model::DimensionSource::Decoded
            {
                pin.dimensions_verified = true;
                pin.width = width;
                pin.height = height;
                pin.dimensions_source = model::DimensionSource::Decoded;
                if pin.dimensions_url.is_none() {
                    pin.dimensions_url = Some(pin.url.clone());
                }
                changed = true;
            }
        }
    }
    if let Some(current) = library.current.as_mut() {
        // The current wallpaper is shown immediately in the UI. Re-read its
        // lightweight image header when a cache file exists so a stale
        // persisted pair cannot survive a replacement of that one file.
        if let Some((width, height)) = wallpaper::cached_dimensions(cache, current) {
            if !current.dimensions_verified
                || current.width != width
                || current.height != height
                || current.dimensions_source != model::DimensionSource::Decoded
            {
                current.dimensions_verified = true;
                current.width = width;
                current.height = height;
                current.dimensions_source = model::DimensionSource::Decoded;
                if current.dimensions_url.is_none() {
                    current.dimensions_url = Some(current.url.clone());
                }
                changed = true;
            }
        }
    }
    changed
}

fn selection_error(failure: wallpaper::SelectionFailure) -> String {
    if failure.cancelled {
        return wallpaper::CHANGE_CANCELLED.into();
    }
    let attempts = failure.attempts;
    let max_attempts = failure.max_attempts;
    let candidates = failure.candidates;
    let success_summary = if failure.source_exhausted {
        format!(
            "{} of {} target images successfully prepared; saved-picture source exhausted",
            failure.successful, failure.target_successes
        )
    } else {
        format!(
            "{} of {} target images successfully prepared",
            failure.successful, failure.target_successes
        )
    };
    let all_filters = !failure.errors.is_empty()
        && failure.errors.iter().all(|error| {
            error.contains("resolution")
                || error.contains("orientation")
                || error.contains("filters")
        });
    let all_network = !failure.errors.is_empty()
        && failure.errors.iter().all(|error| {
            error.contains("download")
                || error.contains("HTTP")
                || error.contains("Pinterest")
                || error.contains("hosted")
                || error.contains("timed out")
                || error.contains("interrupted")
        });
    if failure.exhausted_budget {
        if all_filters {
            return format!(
                "No suitable wallpapers match the resolution/orientation filters after checking {attempts} of {candidates} candidates (limit {max_attempts}; {success_summary}). Wallpaper search paused at its foreground limit; relax the filters or try again."
            );
        }
        if all_network {
            return format!(
                "Wallpaper search paused after checking {attempts} of {candidates} candidates (limit {max_attempts}; {success_summary}) because downloads were unavailable. Retry now; if downloads keep failing, re-import the Pinterest pictures."
            );
        }
        return format!(
            "Wallpaper search paused after checking {attempts} of {candidates} candidates (limit {max_attempts}; {success_summary}). Try again or relax the picture filters."
        );
    }
    if all_filters {
        return format!(
            "No suitable wallpapers match the resolution/orientation filters after checking {attempts} of {candidates} candidates ({success_summary})."
        );
    }
    if all_network {
        return format!(
            "No suitable wallpapers could be downloaded after checking {attempts} of {candidates} candidates ({success_summary}). Retry now; if downloads keep failing, re-import the Pinterest pictures."
        );
    }
    format!(
        "No suitable wallpapers found after checking {attempts} of {candidates} candidates ({success_summary}). Retry now or relax the picture filters."
    )
}
fn exclusive<T>(e: &Engine, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if e.busy.swap(true, Ordering::SeqCst) {
        return Err("Another operation is in progress".into());
    }
    struct Guard<'a>(&'a AtomicBool);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let guard = Guard(&e.busy);
    let result = f();
    *e.error.lock().unwrap() = result.as_ref().err().cloned();
    drop(guard);
    let _ = e.app.emit_to("main", "pinpaper-changed", ());
    if result.is_ok() {
        let _ = e.warm.try_send(());
    }
    result
}
#[tauri::command]
async fn snapshot(
    app: tauri::AppHandle,
    e: tauri::State<'_, Arc<Engine>>,
) -> Result<Snapshot, String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut stored = e.library.lock().unwrap();
        let dimensions_changed = model::canonicalize_dimensions(&mut stored)
            | refresh_cached_dimensions(&e.cache, &mut stored);
        if dimensions_changed {
            if let Err(error) = e.save(&stored) {
                *e.error.lock().unwrap() = Some(error);
            }
        }
        let library = stored.clone();
        drop(stored);
        let preview = library.current.as_ref().and_then(|pin| {
            e.preview
                .lock()
                .unwrap()
                .get_or_load(&wallpaper::cached(&e.cache, pin), preview::load)
        });
        let has_pictures = !library.pins.is_empty();
        let total_pictures = library.pins.len();
        let hidden_pictures = library
            .pins
            .iter()
            .filter(|pin| library.feedback.get(&pin.id) == Some(&-1))
            .count();
        let selected_pictures = library
            .pins
            .iter()
            .filter(|pin| {
                library.settings.board_ids.contains(&pin.board_id)
                    && library.feedback.get(&pin.id) != Some(&-1)
            })
            .count();
        let (available_pictures, unknown_resolution_pictures) = {
            // The current wallpaper is temporarily skipped by rotation, but
            // it still belongs in the user's filter count.
            let mut count_library = library.clone();
            count_library.current = None;
            let available = model::ranked(&count_library);
            let unknown = available
                .iter()
                .filter(|pin| !model::has_known_dimensions(pin))
                .count();
            (available.len(), unknown)
        };
        Snapshot {
            library,
            total_pictures,
            selected_pictures,
            available_pictures,
            unknown_resolution_pictures,
            hidden_pictures,
            connected: e.browser_connected.load(Ordering::SeqCst) || has_pictures,
            browser_connected: e.browser_connected.load(Ordering::SeqCst),
            browser_open: app.get_webview_window(browser_session::LABEL).is_some(),
            busy: e.busy.load(Ordering::SeqCst),
            changing: e.changing.load(Ordering::SeqCst),
            change_status: e.change_status.lock().unwrap().clone(),
            error: e.error.lock().unwrap().clone(),
            preview,
            locale: language::system_language().into(),
        }
    })
    .await
    .map_err(|_| "Could not refresh the app".to_owned())
}
#[tauri::command]
async fn save_settings(e: tauri::State<'_, Arc<Engine>>, settings: Settings) -> Result<(), String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive(&e, || {
            settings.validate()?;
            let previous_launch_at_login = e.library.lock().unwrap().settings.launch_at_login;
            // Update the real OS registration before saving the preference so
            // a portable build and an installed build behave the same way.
            if previous_launch_at_login != settings.launch_at_login {
                autostart::set(settings.launch_at_login)?;
            }
            let mut lib = e.library.lock().unwrap();
            let mut updated = lib.clone();
            updated.settings = settings;
            if let Err(error) = e.save(&updated) {
                if previous_launch_at_login != updated.settings.launch_at_login {
                    let _ = autostart::set(previous_launch_at_login);
                }
                return Err(error);
            }
            *lib = updated;
            Ok(())
        })
    })
    .await
    .map_err(|_| "Settings could not be saved")?
}
#[tauri::command]
fn autostart_status() -> Result<bool, String> {
    autostart::is_enabled()
}
#[tauri::command]
async fn next_wallpaper(e: tauri::State<'_, Arc<Engine>>) -> Result<(), String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || exclusive(&e, || e.next(true)))
        .await
        .map_err(|_| "Wallpaper worker failed")?
}

/// Request cancellation without entering the exclusive-operation gate. The
/// active foreground worker observes this flag between candidates and at each
/// cache commit, while the command itself returns immediately so the UI can
/// remain responsive during a slow network request.
#[tauri::command]
fn stop_wallpaper_change(e: tauri::State<'_, Arc<Engine>>) -> Result<(), String> {
    if e.changing.load(Ordering::SeqCst) {
        e.cancel_change.store(true, Ordering::SeqCst);
        let _ = e.app.emit_to("main", "pinpaper-changed", ());
    }
    Ok(())
}

#[tauri::command]
async fn feedback(e: tauri::State<'_, Arc<Engine>>, value: i8) -> Result<(), String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive(&e, || {
            if ![-1, 0, 1].contains(&value) {
                return Err("Invalid feedback".into());
            }
            {
                let mut lib = e.library.lock().unwrap();
                let id = lib
                    .current
                    .as_ref()
                    .ok_or("No current wallpaper")?
                    .id
                    .clone();
                lib.feedback.insert(id, value);
                e.save(&lib)?;
            }
            if value == -1 {
                e.next(true)?;
            }
            Ok(())
        })
    })
    .await
    .map_err(|_| "Feedback worker failed")?
}
#[tauri::command]
async fn disconnect(app: tauri::AppHandle, e: tauri::State<'_, Arc<Engine>>) -> Result<(), String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive(&e, || {
            browser_session::close_popups(&app);
            if let Some(w) = app.get_webview_window(browser_session::LABEL) {
                w.destroy().map_err(|_| "Cannot close Pinterest window")?;
            }
            // Resetting local data also removes the optional OS login entry;
            // otherwise a fresh default library would disagree with startup.
            autostart::set(false)?;
            e.browser_connected.store(false, Ordering::SeqCst);
            let mut lib = e.library.lock().unwrap();
            *lib = Library::default();
            e.preview.lock().unwrap().clear();
            e.save(&lib)?;
            wallpaper::reset_cache(&e.cache, &e.cache_generation)?;
            wallpaper::clear_all_temporary_unavailable();
            Ok(())
        })
    })
    .await
    .map_err(|_| "Disconnect worker failed")?
}
#[tauri::command]
async fn open_pin(e: tauri::State<'_, Arc<Engine>>, pin_id: Option<String>) -> Result<(), String> {
    let id = {
        let lib = e.library.lock().unwrap();
        match pin_id {
            Some(id) if lib.pins.iter().any(|p| p.id == id) => id,
            Some(_) => return Err("Picture is no longer in your collection".into()),
            None => lib.current.as_ref().ok_or("No current picture")?.id.clone(),
        }
    };
    if id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid pin ID".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        open::that(format!("https://www.pinterest.com/pin/{id}/"))
            .map_err(|_| "Cannot open browser".into())
    })
    .await
    .map_err(|_| "Browser worker failed")?
}
#[tauri::command]
async fn set_pin_hidden(
    e: tauri::State<'_, Arc<Engine>>,
    pin_id: String,
    hidden: bool,
) -> Result<(), String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive(&e, || {
            let mut lib = e.library.lock().unwrap();
            if !lib.pins.iter().any(|p| p.id == pin_id) {
                return Err("Picture is no longer in your collection".into());
            }
            if hidden {
                lib.feedback.insert(pin_id, -1);
            } else {
                lib.feedback.remove(&pin_id);
            }
            e.save(&lib)
        })
    })
    .await
    .map_err(|_| "Picture update failed")?
}
#[tauri::command]
async fn browser_close(
    app: tauri::AppHandle,
    e: tauri::State<'_, Arc<Engine>>,
) -> Result<(), String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive(&e, || {
            browser_session::close_popups(&app);
            if let Some(w) = app.get_webview_window(browser_session::LABEL) {
                w.destroy().map_err(|_| "Cannot close Pinterest")?;
            }
            e.browser_connected.store(false, Ordering::SeqCst);
            Ok(())
        })
    })
    .await
    .map_err(|_| "Browser worker failed")?
}
#[tauri::command]
async fn browser_open(
    app: tauri::AppHandle,
    e: tauri::State<'_, Arc<Engine>>,
) -> Result<(), String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive(&e, || browser_session::open_browser(&app))
    })
    .await
    .map_err(|_| "Browser worker failed")?
}
#[tauri::command]
async fn browser_import(
    app: tauri::AppHandle,
    e: tauri::State<'_, Arc<Engine>>,
) -> Result<usize, String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        exclusive(&e, || {
            // Check the open private window's authentication without persisting cookies.
            let window = browser_session::capture(&app)?;
            e.browser_connected.store(true, Ordering::SeqCst);
            let pins = e.bridge.import(&window)?;
            let count = pins.len();
            let mut lib = e.library.lock().unwrap();
            let mut updated = lib.clone();
            for pin in pins {
                if let Some(existing) = updated
                    .pins
                    .iter_mut()
                    .find(|p| p.id == pin.id && p.board_id == browser_session::SOURCE)
                {
                    let prior = existing.clone();
                    *existing = model::merge_imported_pin(&prior, pin);
                } else {
                    updated.pins.push(pin);
                }
            }
            // Bounded rolling pool; Previously saved collections remain intact.
            let browser_count = updated
                .pins
                .iter()
                .filter(|p| p.board_id == browser_session::SOURCE)
                .count();
            let mut remove = browser_count.saturating_sub(1000);
            updated.pins.retain(|p| {
                if p.board_id == browser_session::SOURCE && remove > 0 {
                    remove -= 1;
                    false
                } else {
                    true
                }
            });
            let new_browser_collection = !updated
                .boards
                .iter()
                .any(|b| b.id == browser_session::SOURCE);
            if !updated
                .boards
                .iter()
                .any(|b| b.id == browser_session::SOURCE)
            {
                updated.boards.push(model::Board {
                    id: browser_session::SOURCE.into(),
                    name: "Browser collection".into(),
                });
            }
            if new_browser_collection
                && !updated
                    .settings
                    .board_ids
                    .iter()
                    .any(|id| id == browser_session::SOURCE)
            {
                updated
                    .settings
                    .board_ids
                    .push(browser_session::SOURCE.into());
            }
            updated.last_sync = chrono::Utc::now().timestamp();
            e.save(&updated)?;
            *lib = updated;
            Ok(count)
        })
    })
    .await
    .map_err(|_| "Import worker failed")?
}
#[tauri::command]
fn browser_report(
    window: tauri::WebviewWindow,
    e: tauri::State<Arc<Engine>>,
    report: browser_session::PageReport,
) -> Result<(), String> {
    e.bridge.receive(window.label(), report)
}

fn main() {
    let commands: Box<dyn Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync> =
        Box::new(tauri::generate_handler![
            snapshot,
            save_settings,
            autostart_status,
            next_wallpaper,
            stop_wallpaper_change,
            feedback,
            disconnect,
            open_pin,
            set_pin_hidden,
            browser_open,
            browser_close,
            browser_import,
            browser_report
        ]);
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let cache = app.path().app_cache_dir()?.join("images");
            let (mut library, error) = match fs::read(data.join("library.json")) {
                Ok(b) => match serde_json::from_slice::<Library>(&b) {
                    Ok(lib) if lib.settings.validate().is_ok() => (lib, None),
                    _ => (
                        Library::default(),
                        Some(
                            "Saved library is invalid. Original file remains until you save."
                                .into(),
                        ),
                    ),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Library::default(), None),
                Err(_) => (Library::default(), Some("Cannot read saved library".into())),
            };
            let mut dimension_migration = model::canonicalize_dimensions(&mut library);
            dimension_migration |= refresh_cached_dimensions(&cache, &mut library);
            let library_for_save = dimension_migration.then(|| library.clone());
            let (warm, warm_receiver) = std::sync::mpsc::sync_channel(1);
            let engine = Arc::new(Engine {
                app: app.handle().clone(),
                library: Mutex::new(library),
                error: Mutex::new(error),
                busy: AtomicBool::new(false),
                changing: AtomicBool::new(false),
                cancel_change: AtomicBool::new(false),
                change_status: Mutex::new(None),
                warm,
                browser_connected: AtomicBool::new(false),
                bridge: browser_session::ImportBridge::default(),
                data,
                cache,
                cache_generation: AtomicU64::new(0),
                preview: Mutex::new(preview::PreviewCache::default()),
            });
            if let Some(library_for_save) = library_for_save {
                if let Err(save_error) = engine.save(&library_for_save) {
                    *engine.error.lock().unwrap() = Some(save_error);
                }
            }
            app.manage(engine.clone());
            if engine.library.lock().unwrap().settings.launch_at_login {
                if let Err(startup_error) = autostart::set(true) {
                    *engine.error.lock().unwrap() = Some(startup_error);
                }
            }
            prefetch::start(&engine, warm_receiver);
            let _ = engine.warm.try_send(());
            #[cfg(target_os = "macos")]
            {
                use objc2_app_kit::{NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification};
                // Workspace retains the observer for the app lifetime. The callback captures
                // only a weak engine reference and never downloads or changes rotation history.
                let weak = Arc::downgrade(&engine);
                let callback = block2::RcBlock::new(
                    move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| {
                        let Some(engine) = weak.upgrade() else { return };
                        let (path, display_mode) = {
                            let library = engine.library.lock().unwrap();
                            (
                                library
                                    .current
                                    .as_ref()
                                    .map(|pin| wallpaper::cached(&engine.cache, pin)),
                                library.settings.display_mode.clone(),
                            )
                        };
                        if let Some(path) = path.filter(|p| p.exists()) {
                            if let Err(error) = wallpaper::apply(&engine.app, &path, &display_mode)
                            {
                                *engine.error.lock().unwrap() = Some(error);
                            }
                        }
                    },
                );
                // Notification is delivered on the posting thread; apply marshals AppKit
                // work to the main thread when necessary.
                let observer = unsafe {
                    NSWorkspace::sharedWorkspace()
                        .notificationCenter()
                        .addObserverForName_object_queue_usingBlock(
                            Some(NSWorkspaceActiveSpaceDidChangeNotification),
                            None,
                            None,
                            &callback,
                        )
                };
                std::mem::forget(observer);
            }
            let lang = language::system_language();
            let show = MenuItem::with_id(
                app,
                "show",
                language::text(lang, "trayOpen"),
                true,
                None::<&str>,
            )?;
            let next = MenuItem::with_id(
                app,
                "next",
                language::text(lang, "changeWallpaper"),
                true,
                None::<&str>,
            )?;
            let pause = MenuItem::with_id(
                app,
                "pause",
                language::text(lang, "trayPause"),
                true,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(
                app,
                "quit",
                language::text(lang, "trayQuit"),
                true,
                None::<&str>,
            )?;
            let menu = Menu::with_items(app, &[&show, &next, &pause, &quit])?;
            TrayIconBuilder::new()
                .icon(tauri::image::Image::from_bytes(
                    if cfg!(target_os = "macos") {
                        include_bytes!("../icons/tray.png")
                    } else {
                        include_bytes!("../icons/tray-color.png")
                    },
                )?)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("Pinpaper")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    let e = app.state::<Arc<Engine>>().inner().clone();
                    match event.id.as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "next" => {
                            std::thread::spawn(move || {
                                let _ = exclusive(&e, || e.next(true));
                            });
                        }
                        "pause" => {
                            std::thread::spawn(move || {
                                let _ = exclusive(&e, || {
                                    let mut l = e.library.lock().unwrap();
                                    l.settings.enabled = !l.settings.enabled;
                                    e.save(&l)
                                });
                            });
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    }
                })
                .build(app)?;
            std::thread::spawn(move || {
                let mut retry_after = 0;
                loop {
                    std::thread::sleep(Duration::from_secs(15));
                    let now = chrono::Utc::now().timestamp();
                    let due = {
                        let lib = engine.library.lock().unwrap();
                        lib.settings.enabled
                            && lib.settings.active(chrono::Local::now().hour())
                            && now - lib.last_change >= lib.settings.interval_minutes as i64 * 60
                    };
                    if due && now >= retry_after && !engine.busy.load(Ordering::SeqCst) {
                        if exclusive(&engine, || engine.next(false)).is_err() {
                            retry_after = now + 300;
                        } else {
                            retry_after = 0;
                        }
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == browser_session::LABEL
                && matches!(event, tauri::WindowEvent::Destroyed)
            {
                browser_session::close_popups(window.app_handle());
                let _ = window.app_handle().emit_to("main", "pinpaper-changed", ());
                if let Some(engine) = window.try_state::<Arc<Engine>>() {
                    engine.browser_connected.store(false, Ordering::SeqCst);
                }
            }
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(move |invoke| {
            let label = invoke.message.webview_ref().label();
            let command = invoke.message.command();
            let allowed = if label == "main" {
                command != "browser_report"
            } else {
                browser_session::session_window(label)
                    && command == "browser_report"
                    && invoke
                        .message
                        .webview_ref()
                        .url()
                        .map(|u| browser_session::trusted_page(&u))
                        .unwrap_or(false)
            };
            if !allowed {
                invoke
                    .resolver
                    .reject("Command is not available to this window");
                return true;
            }
            commands(invoke)
        })
        .run(tauri::generate_context!())
        .expect("error running Pinpaper");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_wallpaper_change_is_registered_and_allowlisted() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main.json"))
                .expect("main capability must be valid JSON");
        let permissions = capability["permissions"]
            .as_array()
            .expect("main capability must declare permissions");
        assert!(permissions
            .iter()
            .any(|permission| permission == "allow-stop-wallpaper-change"));
        assert!(include_str!("../build.rs").contains("\"stop_wallpaper_change\""));
        assert!(include_str!("../permissions/autogenerated/stop_wallpaper_change.toml")
            .contains("commands.allow = [\"stop_wallpaper_change\"]"));
    }

    #[test]
    fn bounded_search_errors_distinguish_network_filters_and_budget() {
        let network = selection_error(wallpaper::SelectionFailure {
            attempts: 2,
            max_attempts: 8,
            candidates: 4,
            successful: 0,
            target_successes: 1,
            exhausted_budget: false,
            source_exhausted: true,
            cancelled: false,
            errors: vec!["Image download returned HTTP 403".into()],
        });
        assert!(network.contains("could be downloaded"));

        let filters = selection_error(wallpaper::SelectionFailure {
            attempts: 2,
            max_attempts: 8,
            candidates: 2,
            successful: 0,
            target_successes: 1,
            exhausted_budget: false,
            source_exhausted: true,
            cancelled: false,
            errors: vec![
                "Downloaded images do not meet your resolution/orientation filters".into(),
            ],
        });
        assert!(filters.contains("match the resolution/orientation filters"));

        let budget = selection_error(wallpaper::SelectionFailure {
            attempts: 3,
            max_attempts: 3,
            candidates: 20,
            successful: 0,
            target_successes: 1,
            exhausted_budget: true,
            source_exhausted: false,
            cancelled: false,
            errors: vec!["Image download failed".into()],
        });
        assert!(budget.contains("Wallpaper search paused"));
        assert!(budget.contains("limit 3"));
    }
}
