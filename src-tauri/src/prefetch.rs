//! A single, event-driven worker warms at most two current candidates.
//! It never applies wallpaper or mutates selection/history. Foreground requests
//! always rank fresh; a superseded download may finish only as an unused cache file.
use crate::{model, wallpaper, Engine};
use std::{
    collections::HashMap,
    sync::{mpsc::Receiver, Arc},
    time::{Duration, Instant},
};

pub fn start(engine: &Arc<Engine>, receiver: Receiver<()>) {
    let weak = Arc::downgrade(engine);
    std::thread::spawn(move || {
        let mut failures = HashMap::new();
        while receiver.recv().is_ok() {
            // Coalesce rapid skips/settings changes; never accumulate a job queue.
            while receiver.try_recv().is_ok() {}
            let Some(engine) = weak.upgrade() else { return };
            let mut attempted = Vec::new();
            for _ in 0..2 {
                if engine.busy.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                // Re-rank between downloads instead of consuming a stale queue.
                let candidate = model::ranked(&engine.library.lock().unwrap())
                    .into_iter()
                    .take(2)
                    .find(|p| !attempted.contains(&p.url));
                let Some(pin) = candidate else { break };
                attempted.push(pin.url.clone());
                if failures
                    .get(&pin.url)
                    .is_some_and(|time: &Instant| time.elapsed() < Duration::from_secs(300))
                {
                    continue;
                }
                let path = wallpaper::cached(&engine.cache, &pin);
                if path.exists() {
                    continue;
                }
                if !wallpaper::has_prefetch_room(&engine.cache) {
                    break;
                }
                if wallpaper::download(&engine.cache, &pin).is_err() {
                    // Background failure is quiet. Manual Next can retry immediately.
                    failures.insert(pin.url.clone(), Instant::now());
                }
                failures.retain(|_, time| time.elapsed() < Duration::from_secs(300));
            }
        }
    });
}
