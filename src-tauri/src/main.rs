#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
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
        atomic::{AtomicBool, Ordering},
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
    warm: std::sync::mpsc::SyncSender<()>,
    browser_connected: AtomicBool,
    bridge: browser_session::ImportBridge,
    data: PathBuf,
    cache: PathBuf,
    preview: Mutex<preview::PreviewCache>,
}
#[derive(Serialize)]
struct Snapshot {
    library: Library,
    connected: bool,
    browser_connected: bool,
    browser_open: bool,
    busy: bool,
    changing: bool,
    error: Option<String>,
    preview: Option<String>,
    locale: String,
}
impl Engine {
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
    fn next(&self) -> Result<(), String> {
        struct Changing<'a>(&'a Engine);
        impl Drop for Changing<'_> {
            fn drop(&mut self) {
                self.0.changing.store(false, Ordering::SeqCst);
                let _ = self.0.app.emit_to("main", "pinpaper-changed", ());
            }
        }
        self.changing.store(true, Ordering::SeqCst);
        let _changing = Changing(self);
        let _ = self.app.emit_to("main", "pinpaper-changed", ());
        let list = model::ranked(&self.library.lock().unwrap());
        if list.is_empty() {
            return Err("No matching pictures. Choose a collection in Pictures to use, add pictures, or relax your picture preferences.".into());
        }
        let settings = self.library.lock().unwrap().settings.clone();
        let selected = wallpaper::first_usable(list, |pin| {
            let path = wallpaper::download(&self.cache, &pin)?;
            let (w, h) = match image::image_dimensions(&path) {
                Ok(size) => size,
                Err(_) => {
                    // Only remove this corrupt file, so a future attempt can fetch it again.
                    let _ = fs::remove_file(&path);
                    return Err("Cached image could not be decoded".into());
                }
            };
            {
                let mut lib = self.library.lock().unwrap();
                for candidate in lib
                    .pins
                    .iter_mut()
                    .filter(|p| p.id == pin.id && p.board_id == pin.board_id)
                {
                    candidate.width = w;
                    candidate.height = h;
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
        });
        // Persist discovered dimensions even when every candidate was unsuitable.
        self.save(&self.library.lock().unwrap())?;
        let (mut pin, path, w, h) = selected?;
        // OS adapter failures are not image failures: stop instead of downloading the library.
        wallpaper::apply(&self.app, &path)?;
        self.preview.lock().unwrap().clear();
        let mut lib = self.library.lock().unwrap();
        pin.width = w;
        pin.height = h;
        lib.current = Some(pin.clone());
        lib.last_change = chrono::Utc::now().timestamp();
        lib.history.push(pin.id);
        if lib.history.len() > 30 {
            lib.history.remove(0);
        }
        self.save(&lib)?;
        Ok(())
    }
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
    let _ = e.warm.try_send(());
    result
}
#[tauri::command]
async fn snapshot(
    app: tauri::AppHandle,
    e: tauri::State<'_, Arc<Engine>>,
) -> Result<Snapshot, String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let library = e.library.lock().unwrap().clone();
        let preview = library.current.as_ref().and_then(|pin| {
            e.preview
                .lock()
                .unwrap()
                .get_or_load(&wallpaper::cached(&e.cache, pin), preview::load)
        });
        let has_pictures = !library.pins.is_empty();
        Snapshot {
            library,
            connected: e.browser_connected.load(Ordering::SeqCst) || has_pictures,
            browser_connected: e.browser_connected.load(Ordering::SeqCst),
            browser_open: app.get_webview_window(browser_session::LABEL).is_some(),
            busy: e.busy.load(Ordering::SeqCst),
            changing: e.changing.load(Ordering::SeqCst),
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
            let mut lib = e.library.lock().unwrap();
            let mut updated = lib.clone();
            updated.settings = settings;
            e.save(&updated)?;
            *lib = updated;
            Ok(())
        })
    })
    .await
    .map_err(|_| "Settings could not be saved")?
}
#[tauri::command]
async fn next_wallpaper(e: tauri::State<'_, Arc<Engine>>) -> Result<(), String> {
    let e = e.inner().clone();
    tauri::async_runtime::spawn_blocking(move || exclusive(&e, || e.next()))
        .await
        .map_err(|_| "Wallpaper worker failed")?
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
                e.next()?;
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
            if let Some(w) = app.get_webview_window(browser_session::LABEL) {
                w.destroy().map_err(|_| "Cannot close Pinterest window")?;
            }
            e.browser_connected.store(false, Ordering::SeqCst);
            let mut lib = e.library.lock().unwrap();
            *lib = Library::default();
            e.preview.lock().unwrap().clear();
            e.save(&lib)?;
            if e.cache.exists() {
                fs::remove_dir_all(&e.cache).map_err(|_| "Signed out, but cache cleanup failed")?;
            }
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
                    *existing = pin;
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
            next_wallpaper,
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
            let (library, error) = match fs::read(data.join("library.json")) {
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
            let (warm, warm_receiver) = std::sync::mpsc::sync_channel(1);
            let engine = Arc::new(Engine {
                app: app.handle().clone(),
                library: Mutex::new(library),
                error: Mutex::new(error),
                busy: AtomicBool::new(false),
                changing: AtomicBool::new(false),
                warm,
                browser_connected: AtomicBool::new(false),
                bridge: browser_session::ImportBridge::default(),
                data,
                cache,
                preview: Mutex::new(preview::PreviewCache::default()),
            });
            app.manage(engine.clone());
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
                        let path = engine
                            .library
                            .lock()
                            .unwrap()
                            .current
                            .as_ref()
                            .map(|pin| wallpaper::cached(&engine.cache, pin));
                        if let Some(path) = path.filter(|p| p.exists()) {
                            if let Err(error) = wallpaper::apply(&engine.app, &path) {
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
                                let _ = exclusive(&e, || e.next());
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
                        if exclusive(&engine, || engine.next()).is_err() {
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
