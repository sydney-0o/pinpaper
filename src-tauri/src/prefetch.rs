//! A single, event-driven worker warms at most two current candidates.
//! It never applies wallpaper or mutates selection/history. Foreground requests
//! always rank fresh; a superseded download may finish only as an unused cache file.
use crate::{model, wallpaper, Engine};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, mpsc::Receiver, Arc},
    time::{Duration, Instant},
};

pub fn start(engine: &Arc<Engine>, receiver: Receiver<()>) {
    let initial_generation = engine.cache_generation.load(Ordering::SeqCst);
    let weak = Arc::downgrade(engine);
    std::thread::spawn(move || {
        let mut failures = HashMap::new();
        let mut generation = initial_generation;
        while receiver.recv().is_ok() {
            // Coalesce rapid skips/settings changes; never accumulate a job queue.
            while receiver.try_recv().is_ok() {}
            let Some(engine) = weak.upgrade() else { return };
            let current_generation = engine.cache_generation.load(Ordering::SeqCst);
            if current_generation != generation {
                failures.clear();
                generation = current_generation;
            }
            let mut attempted = Vec::new();
            for _ in 0..2 {
                if engine.busy.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                // Re-rank between downloads instead of consuming a stale queue.
                let candidate = model::ranked(&engine.library.lock().unwrap())
                    .into_iter()
                    .take(2)
                    .find(|p| {
                        !attempted.contains(&p.url)
                            && !wallpaper::temporarily_unavailable(p)
                    });
                let Some(pin) = candidate else { break };
                attempted.push(pin.url.clone());
                let pin = match engine.prepare_pin(&pin) {
                    Ok(pin) => pin,
                    Err(_) => continue,
                };
                if failures
                    .get(&pin.url)
                    .is_some_and(|time: &Instant| time.elapsed() < Duration::from_secs(300))
                {
                    continue;
                }
                // `cached` understands source-aware provenance. A Pinterest
                // fallback written after an external-source outage must not
                // suppress the next bounded source retry, while a confirmed
                // external download remains reusable.
                let path = wallpaper::cached(&engine.cache, &pin);
                if path.exists() {
                    continue;
                }
                let expected_generation = generation;
                let download = wallpaper::download_with_guard(&engine.cache, &pin, || {
                    engine.cache_generation.load(Ordering::SeqCst) == expected_generation
                });
                match download {
                    Ok(downloaded) => {
                        // Persist the dimensions and exact source variant that
                        // the worker actually cached. A fallback response can
                        // be smaller than Pinterest's original metadata, so a
                        // later foreground change must not infer provenance
                        // from the preferred URL alone.
                        if let Ok((width, height)) = wallpaper::decoded_dimensions(&downloaded.path)
                        {
                            let mut library = engine.library.lock().unwrap();
                            let mut changed = false;
                            let source_url = if downloaded.source_url.is_empty() {
                                library
                                    .pins
                                    .iter()
                                    .find(|stored| {
                                        stored.id == pin.id && stored.board_id == pin.board_id
                                    })
                                    .and_then(|stored| stored.dimensions_url.clone())
                                    .filter(|url| !url.is_empty())
                                    .unwrap_or_else(|| pin.url.clone())
                            } else {
                                downloaded.source_url.clone()
                            };
                            for stored in library.pins.iter_mut().filter(|stored| {
                                stored.id == pin.id && stored.board_id == pin.board_id
                            }) {
                                if !stored.dimensions_verified
                                    || stored.width != width
                                    || stored.height != height
                                    || stored.dimensions_source != model::DimensionSource::Decoded
                                    || stored.dimensions_url.as_deref() != Some(source_url.as_str())
                                {
                                    stored.dimensions_verified = true;
                                    stored.width = width;
                                    stored.height = height;
                                    stored.dimensions_source = model::DimensionSource::Decoded;
                                    stored.dimensions_url = Some(source_url.clone());
                                    changed = true;
                                }
                            }
                            if changed {
                                let _ = engine.save(&library);
                            }
                        }
                    }
                    Err(_) => {
                        // Background failure is quiet. Manual Next can retry immediately.
                        if engine.cache_generation.load(Ordering::SeqCst) == expected_generation {
                            failures.insert(pin.url.clone(), Instant::now());
                        }
                    }
                }
                failures.retain(|_, time| time.elapsed() < Duration::from_secs(300));
            }
        }
    });
}
