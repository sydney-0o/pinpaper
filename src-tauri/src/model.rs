use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Describes the origin of a pair of image dimensions.  A thumbnail's
/// dimensions are useful for diagnostics, but they are never a claim about
/// the largest source that Pinterest can provide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DimensionSource {
    #[default]
    Unknown,
    Thumbnail,
    PinterestOriginal,
    Decoded,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub interval_minutes: u64,
    pub active_start: u32,
    pub active_end: u32,
    pub enabled: bool,
    /// Register the packaged app with the user's desktop login/startup list.
    /// This is intentionally opt-in and independent from wallpaper rotation.
    pub launch_at_login: bool,
    pub keywords: String,
    pub exclude: String,
    pub orientation: String,
    pub min_width: u32,
    /// How the operating system should place the selected image on each display.
    /// Older libraries deserialize the missing field to the quality-preserving
    /// fill default.
    pub display_mode: String,
    pub board_ids: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_minutes: 60,
            active_start: 8,
            active_end: 23,
            enabled: false,
            launch_at_login: false,
            keywords: String::new(),
            exclude: String::new(),
            orientation: "landscape".into(),
            min_width: 1280,
            display_mode: "fill".into(),
            board_ids: vec![],
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=10080).contains(&self.interval_minutes)
            || self.active_start > 23
            || self.active_end > 23
            || self.min_width > 16384
            || !["fill", "fit", "center", "tile", "stretch"].contains(&self.display_mode.as_str())
            || self.keywords.len() > 1000
            || self.exclude.len() > 1000
            || self.board_ids.len() > 100
            || !["landscape", "portrait", "any"].contains(&self.orientation.as_str())
        {
            return Err("Invalid settings".into());
        }
        Ok(())
    }
    pub fn active(&self, hour: u32) -> bool {
        if self.active_start == self.active_end {
            true
        } else if self.active_start < self.active_end {
            hour >= self.active_start && hour < self.active_end
        } else {
            hour >= self.active_start || hour < self.active_end
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Pin {
    #[serde(default)]
    pub dimensions_verified: bool,
    pub id: String,
    pub board_id: String,
    pub title: String,
    pub description: String,
    pub url: String,
    /// Outbound URL recorded by Pinterest for the pin.  When it points at a
    /// public source page, the downloader may use that page's exact image
    /// URL before falling back to Pinterest's CDN copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// An image URL observed on Pinterest and kept as a fallback when the
    /// preferred observed URL is no longer available from the CDN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_url: Option<String>,
    /// Whether `url` was explicitly exposed as Pinterest's original image
    /// source by the page capture. A `/originals/` path alone is insufficient:
    /// older versions guessed that path from a thumbnail and the extension can
    /// differ from the real original (for example JPG thumbnail, PNG source).
    #[serde(default)]
    pub original_url_exact: bool,
    /// Width and height of the decoded, orientation-normalized image. These
    /// values are usable for filtering only when `dimensions_verified` is
    /// true. Older libraries deserialize missing quality metadata safely.
    pub width: u32,
    pub height: u32,
    /// Dimensions observed for the rendered Pinterest thumbnail. They are
    /// deliberately kept separate from the maximum source dimensions.
    #[serde(default)]
    pub thumbnail_width: u32,
    #[serde(default)]
    pub thumbnail_height: u32,
    /// Pinterest metadata dimensions for the matching original source, when
    /// the page reported them. These are a pre-download filter hint, not a
    /// replacement for decoded dimensions after a fallback download.
    #[serde(default)]
    pub max_width: u32,
    #[serde(default)]
    pub max_height: u32,
    #[serde(default)]
    pub max_dimensions_source: DimensionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_dimensions_url: Option<String>,
    /// Source of the current width/height pair. The bool above remains for
    /// compatibility with the original on-disk format.
    #[serde(default)]
    pub dimensions_source: DimensionSource,
    /// Exact URL variant whose decoded pixels produced `width`/`height`.
    /// This prevents a fallback thumbnail's dimensions from being presented
    /// as dimensions of a different original variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions_url: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Board {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub settings: Settings,
    pub boards: Vec<Board>,
    pub pins: Vec<Pin>,
    pub feedback: HashMap<String, i8>,
    pub history: Vec<String>,
    /// Stable image keys already used in the current rotation history.
    /// Keeping this separately from the short display history lets a round
    /// span more than thirty pins, deduplicate repins and survive an
    /// application restart. Legacy board/pin keys are still recognized.
    #[serde(default)]
    pub rotation_seen: Vec<String>,
    /// Stable source-aware keys already inspected and rejected in the current
    /// rotation round. Rejected pictures stay separate from successful ones:
    /// they must not be shown again on every click, but they also must not be
    /// treated as successful wallpapers when deciding whether a round ended.
    #[serde(default)]
    pub rotation_attempted: Vec<String>,
    /// Whether the new persisted rotation state has observed at least one
    /// post-migration attempt. This keeps the legacy short history useful only
    /// during the first round after an upgrade.
    #[serde(default)]
    pub rotation_initialized: bool,
    /// Candidate-filter signature that started the current round. Settings
    /// changes can alter the visible pool without making a subset look like a
    /// completed full round.
    #[serde(default)]
    pub rotation_filter_signature: String,
    pub current: Option<Pin>,
    pub last_change: i64,
    pub last_sync: i64,
}
pub fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.chars().count() > 2)
        .map(str::to_owned)
        .collect()
}

/// Return dimensions that can safely be used before downloading a candidate.
/// Verified decoded pixels take precedence over a metadata estimate because a
/// Pinterest original may have fallen back to a smaller observed URL.
pub fn filter_dimensions(pin: &Pin) -> Option<(u32, u32)> {
    if pin.dimensions_verified && pin.width > 0 && pin.height > 0 {
        return Some((pin.width, pin.height));
    }
    if pin.max_dimensions_source == DimensionSource::PinterestOriginal
        && pin.max_width > 0
        && pin.max_height > 0
        && pin.max_dimensions_url.as_deref().is_some_and(|url| {
            crate::network::valid_image_url(url) && crate::network::same_asset(url, &pin.url)
        })
    {
        return Some((pin.max_width, pin.max_height));
    }
    None
}

pub fn has_known_dimensions(pin: &Pin) -> bool {
    filter_dimensions(pin).is_some()
}

fn canonicalize_pin_dimensions(pin: &mut Pin) -> bool {
    let mut changed = false;

    if pin
        .source_url
        .as_deref()
        .is_some_and(|url| !crate::network::valid_source_url(url))
    {
        pin.source_url = None;
        changed = true;
    }
    if pin.original_url_exact && !crate::network::valid_image_url(&pin.url) {
        pin.original_url_exact = false;
        changed = true;
    }
    if pin
        .fallback_url
        .as_deref()
        .is_some_and(|url| !crate::network::same_asset(url, &pin.url))
    {
        pin.fallback_url = None;
        changed = true;
    }

    // `width`/`height` are the dimensions of pixels that Pinpaper decoded and
    // cached. Older library files used these fields for Pinterest page
    // metadata, so an unverified pair must never reach filtering or the UI.
    if pin.dimensions_verified && pin.width > 0 && pin.height > 0 {
        if pin.dimensions_url.is_none() {
            // Prior versions only recorded the decoded dimensions. Their
            // successful wallpaper path was the preferred URL, so retain that
            // legacy fact until a newer download records a more exact source.
            pin.dimensions_url = Some(pin.url.clone());
            changed = true;
        }
        let decoded_asset_matches = match (
            pin.dimensions_url
                .as_deref()
                .and_then(crate::network::asset_identity),
            crate::network::asset_identity(&pin.url),
        ) {
            (Some(decoded_asset), Some(pin_asset)) => decoded_asset == pin_asset,
            _ => pin
                .source_url
                .as_deref()
                .is_some_and(crate::network::valid_source_url)
                && pin
                    .dimensions_url
                    .as_deref()
                    .is_some_and(crate::network::valid_source_url),
        };
        if !decoded_asset_matches {
            pin.dimensions_verified = false;
            pin.width = 0;
            pin.height = 0;
            pin.dimensions_source = DimensionSource::Unknown;
            pin.dimensions_url = None;
            changed = true;
        } else if pin.dimensions_source != DimensionSource::Decoded {
            pin.dimensions_source = DimensionSource::Decoded;
            changed = true;
        }
    } else {
        if pin.dimensions_verified || pin.width != 0 || pin.height != 0 {
            pin.dimensions_verified = false;
            pin.width = 0;
            pin.height = 0;
            changed = true;
        }
        if pin.dimensions_url.is_some() {
            pin.dimensions_url = None;
            changed = true;
        }
        if pin.dimensions_source != DimensionSource::Unknown {
            pin.dimensions_source = DimensionSource::Unknown;
            changed = true;
        }
    }

    // A partial pair cannot describe a usable raster. Keep the observation,
    // only when both axes came from the same rendered thumbnail.
    if (pin.thumbnail_width == 0) != (pin.thumbnail_height == 0) {
        pin.thumbnail_width = 0;
        pin.thumbnail_height = 0;
        changed = true;
    }

    let max_matches_url = pin.max_dimensions_source == DimensionSource::PinterestOriginal
        && pin.max_width > 0
        && pin.max_height > 0
        && pin.max_dimensions_url.as_deref().is_some_and(|url| {
            crate::network::valid_image_url(url) && crate::network::same_asset(url, &pin.url)
        });
    if !max_matches_url
        && (pin.max_width != 0
            || pin.max_height != 0
            || pin.max_dimensions_source != DimensionSource::Unknown
            || pin.max_dimensions_url.is_some())
    {
        pin.max_width = 0;
        pin.max_height = 0;
        pin.max_dimensions_source = DimensionSource::Unknown;
        pin.max_dimensions_url = None;
        changed = true;
    }

    changed
}

/// Repair records written by older versions and enforce the dimension
/// provenance invariant before a library is used. This does not invent
/// dimensions: a stale unverified pair is cleared and is replaced later by
/// the dimensions read from the cached image.
pub fn canonicalize_dimensions(lib: &mut Library) -> bool {
    let mut changed = false;
    for pin in &mut lib.pins {
        changed |= canonicalize_pin_dimensions(pin);
    }
    if let Some(current) = lib.current.as_mut() {
        changed |= canonicalize_pin_dimensions(current);
    }

    // Re-imports can update a pin's URL variant while `current` still holds
    // the old copy. Copy only the canonical dimension fields for the same
    // image identity; titles, URLs and fallbacks remain owned by `current`.
    let current_fields = lib.current.as_ref().and_then(|current| {
        let current_is_valid =
            current.dimensions_verified && current.width > 0 && current.height > 0;
        (!current_is_valid)
            .then(|| {
                lib.pins
                    .iter()
                    .find(|pin| {
                        pin.dimensions_verified
                            && pin.width > 0
                            && pin.height > 0
                            && pin.id == current.id
                            && pin.board_id == current.board_id
                            && image_identity(pin) == image_identity(current)
                    })
                    .map(|pin| {
                        (
                            pin.dimensions_verified,
                            pin.width,
                            pin.height,
                            pin.dimensions_source,
                            pin.dimensions_url.clone(),
                        )
                    })
            })
            .flatten()
    });
    if let (Some(current), Some((verified, width, height, source, dimensions_url))) =
        (lib.current.as_mut(), current_fields)
    {
        if current.dimensions_verified != verified
            || current.width != width
            || current.height != height
            || current.dimensions_source != source
            || current.dimensions_url != dimensions_url
        {
            current.dimensions_verified = verified;
            current.width = width;
            current.height = height;
            current.dimensions_source = source;
            current.dimensions_url = dimensions_url;
            changed = true;
        }
    }
    changed
}

/// Merge a fresh Pinterest observation into an existing pin. Re-importing a
/// thumbnail or a URL variant for the same underlying asset keeps decoded
/// quality and a previously confirmed maximum. A genuinely new asset clears
/// those values by taking the fresh pin as-is.
pub fn merge_imported_pin(existing: &Pin, mut incoming: Pin) -> Pin {
    if image_identity(existing) != image_identity(&incoming) {
        return incoming;
    }
    // Keep a page-confirmed original source when a later feed observation only
    // exposes a thumbnail. The thumbnail remains a fallback, so the exact
    // source and its metadata stay attached to the same asset.
    if existing.original_url_exact && !incoming.original_url_exact {
        let observed = incoming.url.clone();
        incoming.url = existing.url.clone();
        incoming.original_url_exact = true;
        if observed != incoming.url && incoming.fallback_url.is_none() {
            incoming.fallback_url = Some(observed);
        }
    }
    if incoming.source_url.is_none() {
        // Feed observations often omit `link` even when the detail payload
        // exposed it earlier. Preserve the stronger source provenance until
        // Pinterest reports a different outbound page explicitly.
        incoming.source_url = existing.source_url.clone();
    }

    // The exact-original promotion above intentionally makes a later
    // thumbnail observation refer to the same source that was already
    // decoded. Decide whether dimensions can be retained only after that
    // normalization; a genuinely different URL variant still forces a fresh
    // decode.
    let source_changed = existing.url != incoming.url || existing.source_url != incoming.source_url;

    // Imports describe what Pinterest reported on the page. They do not
    // replace dimensions decoded from a file that Pinpaper already accepted,
    // even if a future report happens to contain a different pair.
    if existing.dimensions_verified && !source_changed {
        incoming.dimensions_verified = true;
        incoming.width = existing.width;
        incoming.height = existing.height;
        incoming.dimensions_source = DimensionSource::Decoded;
        incoming.dimensions_url = existing.dimensions_url.clone();
    } else {
        incoming.dimensions_verified = false;
        incoming.width = 0;
        incoming.height = 0;
        incoming.dimensions_source = DimensionSource::Unknown;
        incoming.dimensions_url = None;
    }

    // Preserve useful observations that a partial page report omitted.
    if incoming.thumbnail_width == 0 || incoming.thumbnail_height == 0 {
        incoming.thumbnail_width = existing.thumbnail_width;
        incoming.thumbnail_height = existing.thumbnail_height;
    }
    if incoming.fallback_url.is_none() {
        incoming.fallback_url = existing.fallback_url.clone();
    }

    let incoming_max_matches = incoming.max_dimensions_source == DimensionSource::PinterestOriginal
        && incoming.max_width > 0
        && incoming.max_height > 0
        && incoming.max_dimensions_url.as_deref().is_some_and(|url| {
            crate::network::valid_image_url(url) && crate::network::same_asset(url, &incoming.url)
        });
    let existing_max_matches = existing.max_dimensions_source == DimensionSource::PinterestOriginal
        && existing.max_width > 0
        && existing.max_height > 0
        && existing.max_dimensions_url.as_deref().is_some_and(|url| {
            crate::network::valid_image_url(url) && crate::network::same_asset(url, &existing.url)
        });
    if existing_max_matches
        && (!incoming_max_matches
            || existing.max_width.saturating_mul(existing.max_height)
                > incoming.max_width.saturating_mul(incoming.max_height))
    {
        incoming.max_width = existing.max_width;
        incoming.max_height = existing.max_height;
        incoming.max_dimensions_source = DimensionSource::PinterestOriginal;
        incoming.max_dimensions_url = existing.max_dimensions_url.clone();
    }
    incoming
}

fn rotation_filter_signature(settings: &Settings) -> String {
    // Schedule and display settings do not affect candidate membership and
    // therefore must not create a new rotation context.
    serde_json::to_string(&(
        &settings.keywords,
        &settings.exclude,
        &settings.orientation,
        settings.min_width,
        &settings.board_ids,
    ))
    .unwrap_or_default()
}

fn matches_geometry(pin: &Pin, settings: &Settings) -> bool {
    // Previously cached 236x/736x pixels must not exclude an untried original.
    // The downloaded file is checked against the filters again before use.
    if crate::network::is_resized_url(&pin.url) && crate::network::needs_pin_resolution(pin) {
        return true;
    }
    // A pin with an unverified external source remains eligible even when its
    // last Pinterest fallback was too small. The source resolver runs during
    // preparation and may replace that fallback with a larger exact file;
    // filtering it out here would make the upgrade impossible.
    let verified_external = pin.dimensions_verified
        && pin.dimensions_source == DimensionSource::Decoded
        && pin
            .dimensions_url
            .as_deref()
            .is_some_and(|url| crate::network::valid_source_url(url));
    if pin.source_url.is_some() && !verified_external {
        return true;
    }
    if pin.dimensions_verified && pin.width > 0 && pin.height > 0 {
        return pin.width >= settings.min_width
            && match settings.orientation.as_str() {
                "landscape" => pin.width > pin.height,
                "portrait" => pin.height > pin.width,
                _ => true,
            };
    }
    if pin.max_dimensions_source == DimensionSource::PinterestOriginal
        && pin.max_width > 0
        && pin.max_height > 0
        && pin.max_dimensions_url.as_deref().is_some_and(|url| {
            crate::network::valid_image_url(url) && crate::network::same_asset(url, &pin.url)
        })
    {
        // Pinterest metadata may describe the encoded raster before EXIF
        // orientation is applied. Use the larger axis for the pre-download
        // width gate and keep either non-square orientation viable; the
        // decoded, oriented image remains the final authority.
        let possible_width = pin.max_width.max(pin.max_height);
        return possible_width >= settings.min_width
            && match settings.orientation.as_str() {
                "landscape" | "portrait" => pin.max_width != pin.max_height,
                _ => true,
            };
    }
    // Unknown dimensions remain candidates. The foreground prepare step
    // downloads and verifies them within the selected saved-picture pool.
    true
}

pub fn ranked(lib: &Library) -> Vec<Pin> {
    let wanted = words(&lib.settings.keywords);
    let excluded = words(&lib.settings.exclude);
    let current_image = lib.current.as_ref().map(image_identity);
    let mut results: Vec<(f64, Pin)> = lib
        .pins
        .iter()
        .filter(|p| {
            let tokens = words(&format!("{} {}", p.title, p.description));
            lib.settings.board_ids.contains(&p.board_id)
                && lib.feedback.get(&p.id) != Some(&-1)
                && current_image
                    .as_ref()
                    .map(|current| current != &image_identity(p))
                    .unwrap_or(true)
                && matches_geometry(p, &lib.settings)
                && !excluded.iter().any(|w| tokens.contains(w))
        })
        .map(|p| {
            let tokens = words(&format!("{} {}", p.title, p.description));
            // Rotation is applied after ranking. Keeping the ranking stable
            // within a round means preferred keywords retain their order and
            // a new round starts predictably after every eligible pin was used.
            let score = wanted.iter().filter(|w| tokens.contains(w)).count() as f64 * 4.0;
            (score, p.clone())
        })
        .collect();
    results.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    // Pinterest can expose the same asset through several pins and several
    // CDN size paths. Keep one candidate for the underlying image so a
    // repin cannot make the rotation appear to loop early.
    let mut images = HashSet::new();
    results
        .into_iter()
        .filter(|(_, pin)| images.insert(image_identity(pin)))
        .map(|(_, p)| p)
        .collect()
}

/// Return a stable identity for the downloaded image, independent of the
/// Pinterest CDN size path. Invalid or synthetic URLs fall back to the pin
/// identity so test/legacy records do not collapse into one empty key.
pub fn image_identity(pin: &Pin) -> String {
    if let Some(asset) = crate::network::asset_identity(&pin.url) {
        format!("url:{asset}")
    } else {
        format!("pin:{}:{}", pin.board_id, pin.id)
    }
}

fn legacy_rotation_key(pin: &Pin) -> String {
    format!("{}:{}", pin.board_id, pin.id)
}

pub fn rotation_key(pin: &Pin) -> String {
    format!("image:{}", image_identity(pin))
}

/// Include the observed source URL so an import that repairs a pin's source
/// makes that pin eligible again without resetting the rest of the round.
pub fn rotation_attempt_key(pin: &Pin) -> String {
    // Keep the pin key as the prefix for migration compatibility. Ranked
    // candidates are already deduplicated by image identity, so this remains
    // source-aware without making an old attempted cursor disappear.
    format!("{}|{}", legacy_rotation_key(pin), pin.url)
}

fn seen_keys(lib: &Library) -> HashSet<String> {
    lib.rotation_seen.iter().cloned().collect()
}

fn attempted_keys(lib: &Library) -> HashSet<String> {
    lib.rotation_attempted.iter().cloned().collect()
}

fn was_seen_with(lib: &Library, pin: &Pin, seen: &HashSet<String>) -> bool {
    let key = rotation_key(pin);
    let legacy_key = legacy_rotation_key(pin);
    seen.contains(&key)
        || seen.contains(&legacy_key)
        // Libraries written before rotation_seen was introduced only have a
        // short history. Treat it as already used during migration so the
        // first round after an upgrade does not immediately repeat pictures.
        || (!lib.rotation_initialized && lib.history.iter().any(|seen| seen == &pin.id))
}

fn was_attempted_with(pin: &Pin, attempted: &HashSet<String>) -> bool {
    attempted.contains(&rotation_attempt_key(pin))
}

/// Return candidates in rank order, using every unseen eligible picture before
/// starting another pass through the collection. Current, hidden, source and
/// preference filters are all applied by `ranked` first.
#[cfg(test)]
fn rotation_candidates(lib: &Library) -> Vec<Pin> {
    let ranked = ranked(lib);
    let seen = seen_keys(lib);
    if ranked.iter().any(|pin| !was_seen_with(lib, pin, &seen)) {
        ranked
            .into_iter()
            .filter(|pin| !was_seen_with(lib, pin, &seen))
            .collect()
    } else {
        ranked
    }
}

/// Return the foreground search pool with never-inspected pins first.
///
/// Failed candidates are skipped for the rest of this round, while successful
/// candidates remain a bounded fallback when only the tail is left. The caller
/// starts a new round only after every currently eligible candidate has been
/// inspected.
pub fn foreground_rotation_candidates(lib: &Library) -> Vec<Pin> {
    let ranked = ranked(lib);
    let seen_keys = seen_keys(lib);
    let attempted_keys = attempted_keys(lib);
    let mut fresh = Vec::new();
    let mut seen = Vec::new();
    for pin in ranked {
        if !was_seen_with(lib, &pin, &seen_keys) && !was_attempted_with(&pin, &attempted_keys) {
            fresh.push(pin);
        } else if was_seen_with(lib, &pin, &seen_keys) {
            // A successful picture is a bounded fallback only after all
            // never-inspected pictures have been tried in this round.
            seen.push(pin);
        }
    }
    fresh.extend(seen);
    fresh
}

/// Remove attempted keys for pins whose observed source no longer exists.
/// Re-importing a changed pin therefore repairs that pin without restarting
/// the rest of the current round.
fn prune_rotation_attempts(lib: &mut Library) {
    let current_keys: std::collections::HashSet<_> =
        lib.pins.iter().map(rotation_attempt_key).collect();
    lib.rotation_attempted
        .retain(|attempted| current_keys.contains(attempted));
}

/// Mark a candidate as inspected in the current round. This is intentionally
/// separate from `record_rotation`: a failed download/filter check advances
/// the search cursor but does not count as a successful wallpaper.
pub fn record_rotation_attempt(lib: &mut Library, pin: &Pin) {
    lib.rotation_initialized = true;
    if lib.rotation_filter_signature.is_empty() {
        lib.rotation_filter_signature = rotation_filter_signature(&lib.settings);
    }
    let key = rotation_attempt_key(pin);
    if !lib
        .rotation_attempted
        .iter()
        .any(|attempted| attempted == &key)
    {
        lib.rotation_attempted.push(key);
    }
    prune_rotation_attempts(lib);
}

/// Start a new round only after every currently eligible pin has been
/// inspected. Failed pins are included in this exhaustion test, while a
/// fallback to a successful pin alone never resets the round.
pub fn begin_rotation_round(lib: &mut Library) -> bool {
    prune_rotation_attempts(lib);
    if !lib.rotation_initialized && !lib.rotation_seen.is_empty() {
        // Libraries written before this marker existed already have a current
        // success round. Keep that state and mark the migration complete.
        lib.rotation_initialized = true;
    }
    // A library written before the persistent round fields existed has only
    // the short display history. Keep that migration history as a fallback
    // until the first new successful change records the new state shape.
    if !lib.rotation_initialized
        && lib.rotation_seen.is_empty()
        && lib.rotation_attempted.is_empty()
    {
        if lib.rotation_filter_signature.is_empty() {
            lib.rotation_filter_signature = rotation_filter_signature(&lib.settings);
        }
        return false;
    }
    let current_signature = rotation_filter_signature(&lib.settings);
    if lib.rotation_filter_signature.is_empty() {
        lib.rotation_filter_signature = current_signature.clone();
    }
    // Filter changes keep any remaining eligible tail. Once that tail is
    // exhausted, start the next round under the active filters, even when the
    // saved round began with different settings. Otherwise a stale signature
    // permanently traps selection in the successful fallback list.
    let eligible = ranked(lib);
    let seen = seen_keys(lib);
    let attempted = attempted_keys(lib);
    if eligible.is_empty()
        || !eligible
            .iter()
            .all(|pin| was_seen_with(lib, pin, &seen) || was_attempted_with(pin, &attempted))
    {
        return false;
    }
    lib.rotation_seen.clear();
    lib.rotation_attempted.clear();
    lib.rotation_filter_signature = current_signature;
    true
}

/// Mark a pin as used only after the operating system accepted it as the new
/// wallpaper. Stale keys are pruned so repeated imports do not grow the file
/// forever while still preserving the round for all current pins.
pub fn record_rotation(lib: &mut Library, pin: &Pin) {
    record_rotation_attempt(lib, pin);
    let key = rotation_key(pin);
    let legacy_key = legacy_rotation_key(pin);
    lib.rotation_seen.retain(|seen| seen != &legacy_key);
    if !lib.rotation_seen.iter().any(|seen| seen == &key) {
        lib.rotation_seen.push(key);
    }
    let current_keys: HashSet<_> = lib
        .pins
        .iter()
        .flat_map(|pin| [rotation_key(pin), legacy_rotation_key(pin)])
        .collect();
    lib.rotation_seen.retain(|seen| current_keys.contains(seen));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_wraps_midnight() {
        let s = Settings {
            active_start: 22,
            active_end: 7,
            ..Default::default()
        };
        assert!(s.active(23));
        assert!(s.active(0));
        assert!(!s.active(7));
        assert!(!s.active(12));
    }
    #[test]
    fn equal_hours_is_all_day() {
        let s = Settings {
            active_start: 0,
            active_end: 0,
            ..Default::default()
        };
        assert!((0..24).all(|h| s.active(h)));
    }
    fn pin(id: &str, title: &str) -> Pin {
        Pin {
            dimensions_verified: true,
            id: id.into(),
            board_id: "b".into(),
            title: title.into(),
            description: String::new(),
            url: String::new(),
            fallback_url: None,
            width: 1920,
            height: 1080,
            ..Default::default()
        }
    }
    #[test]
    fn source_selection_and_hidden_pictures_are_respected() {
        let mut lib = Library::default();
        let first = pin("1", "forest");
        let mut second = pin("2", "ocean");
        second.board_id = "other".into();
        lib.pins = vec![first, second];
        assert!(ranked(&lib).is_empty());
        lib.settings.board_ids = vec!["other".into()];
        assert_eq!(
            ranked(&lib)
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            vec!["2"]
        );
        lib.feedback.insert("2".into(), -1);
        assert!(ranked(&lib).is_empty());
        lib.feedback.remove("2");
        assert_eq!(ranked(&lib).len(), 1);
        lib.settings.board_ids.clear();
        assert!(ranked(&lib).is_empty());
    }
    #[test]
    fn ranking_respects_feedback_filters_and_keeps_keyword_order() {
        let mut l = Library::default();
        l.settings.board_ids = vec!["b".into()];
        l.settings.keywords = "forest".into();
        l.pins = vec![pin("1", "ocean"), pin("2", "forest"), pin("3", "forest")];
        l.feedback.insert("3".into(), -1);
        assert_eq!(ranked(&l)[0].id, "2");
        assert_eq!(ranked(&l).len(), 2);
        l.history.push("2".into());
        assert_eq!(ranked(&l)[0].id, "2");
        l.settings.exclude = "forest".into();
        assert_eq!(ranked(&l).len(), 1);
        l.settings.min_width = 4000;
        assert!(ranked(&l).is_empty());
    }

    #[test]
    fn rotation_uses_every_unseen_pin_before_starting_next_round() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.pins = (0..40)
            .map(|index| pin(&index.to_string(), "landscape"))
            .collect();
        assert_eq!(rotation_candidates(&lib).len(), 40);

        for index in 0..35 {
            let pin = lib.pins[index].clone();
            record_rotation(&mut lib, &pin);
        }
        let remaining: Vec<_> = rotation_candidates(&lib)
            .into_iter()
            .map(|pin| pin.id)
            .collect();
        assert_eq!(remaining.len(), 5);
        assert_eq!(remaining[0], "35");

        for index in 35..40 {
            let pin = lib.pins[index].clone();
            record_rotation(&mut lib, &pin);
        }
        assert_eq!(rotation_candidates(&lib).len(), 40);
    }

    #[test]
    fn rotation_state_survives_serialization_and_respects_filters() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.pins = vec![pin("1", "one"), pin("2", "two"), pin("3", "three")];
        let used = lib.pins[0].clone();
        record_rotation(&mut lib, &used);
        let restored: Library = serde_json::from_slice(&serde_json::to_vec(&lib).unwrap()).unwrap();
        assert_eq!(rotation_candidates(&restored)[0].id, "2");

        let mut filtered = restored;
        filtered.feedback.insert("2".into(), -1);
        filtered.settings.board_ids = vec!["other".into()];
        assert!(rotation_candidates(&filtered).is_empty());
        filtered.settings.board_ids = vec!["b".into()];
        assert_eq!(rotation_candidates(&filtered)[0].id, "3");
    }

    #[test]
    fn foreground_rotation_falls_through_from_an_unseen_tail_to_next_round() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.pins = (0..10)
            .map(|index| pin(&index.to_string(), "landscape"))
            .collect();
        for index in 0..8 {
            let used = lib.pins[index].clone();
            record_rotation(&mut lib, &used);
        }

        let ids: Vec<_> = foreground_rotation_candidates(&lib)
            .into_iter()
            .map(|pin| pin.id)
            .collect();
        assert_eq!(
            ids,
            (8..10)
                .chain(0..8)
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rotation_record_only_keeps_current_library_keys() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.pins = vec![pin("1", "one")];
        lib.rotation_seen = vec!["b:1".into(), "b:removed".into()];
        let pin = lib.pins[0].clone();
        record_rotation(&mut lib, &pin);
        assert_eq!(lib.rotation_seen, vec![rotation_key(&pin)]);
        assert_eq!(lib.rotation_attempted, vec![rotation_attempt_key(&pin)]);
    }

    #[test]
    fn failed_attempts_advance_without_becoming_successful_history() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.pins = (0..5)
            .map(|index| pin(&index.to_string(), "landscape"))
            .collect();

        let successful = lib.pins[0].clone();
        record_rotation(&mut lib, &successful);
        let failed = lib.pins[1].clone();
        record_rotation_attempt(&mut lib, &failed);

        let ids: Vec<_> = foreground_rotation_candidates(&lib)
            .into_iter()
            .map(|candidate| candidate.id)
            .collect();
        assert_eq!(ids, vec!["2", "3", "4", "0"]);
        assert!(lib.rotation_seen.contains(&rotation_key(&successful)));
        assert!(!lib.rotation_seen.contains(&rotation_key(&failed)));
        assert!(!begin_rotation_round(&mut lib));
        assert_eq!(lib.rotation_seen.len(), 1);
        assert_eq!(lib.rotation_attempted.len(), 2);
    }

    #[test]
    fn round_resets_only_after_all_current_candidates_were_inspected() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.pins = (0..3)
            .map(|index| pin(&index.to_string(), "landscape"))
            .collect();
        for candidate in lib.pins.clone() {
            record_rotation_attempt(&mut lib, &candidate);
        }
        assert!(begin_rotation_round(&mut lib));
        assert!(lib.rotation_seen.is_empty());
        assert!(lib.rotation_attempted.is_empty());
    }

    #[test]
    fn changing_one_source_does_not_reset_other_rotation_state() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.pins = (0..3)
            .map(|index| pin(&index.to_string(), "landscape"))
            .collect();
        let first = lib.pins[0].clone();
        record_rotation_attempt(&mut lib, &first);
        let old_key = rotation_attempt_key(&lib.pins[0]);
        lib.pins[0].url = "https://new.example/image.jpg".into();
        let second_key = rotation_attempt_key(&lib.pins[1]);
        let second = lib.pins[1].clone();
        record_rotation_attempt(&mut lib, &second);

        assert!(!lib.rotation_attempted.contains(&old_key));
        assert!(lib.rotation_attempted.contains(&second_key));
        assert!(foreground_rotation_candidates(&lib)
            .iter()
            .any(|candidate| candidate.id == "0"));
    }

    #[test]
    fn legacy_history_does_not_block_the_first_full_round_after_migration() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.pins = (0..4)
            .map(|index| pin(&index.to_string(), "landscape"))
            .collect();
        lib.history = vec!["0".into(), "1".into(), "2".into()];

        let migrated_ids: Vec<_> = foreground_rotation_candidates(&lib)
            .into_iter()
            .map(|candidate| candidate.id)
            .collect();
        assert_eq!(migrated_ids, vec!["3", "0", "1", "2"]);

        for candidate in lib.pins.clone() {
            record_rotation(&mut lib, &candidate);
        }
        assert!(begin_rotation_round(&mut lib));
        let fresh_ids: Vec<_> = foreground_rotation_candidates(&lib)
            .into_iter()
            .map(|candidate| candidate.id)
            .collect();
        assert_eq!(fresh_ids, vec!["0", "1", "2", "3"]);
    }

    #[test]
    fn image_identity_deduplicates_repins_and_excludes_the_current_asset() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.settings.orientation = "any".into();
        lib.settings.min_width = 0;
        lib.pins = vec![
            Pin {
                dimensions_verified: true,
                id: "1".into(),
                board_id: "b".into(),
                title: "large".into(),
                description: String::new(),
                url: "https://i.pinimg.com/736x/hash.jpg".into(),
                fallback_url: None,
                width: 1920,
                height: 1080,
                ..Default::default()
            },
            Pin {
                dimensions_verified: true,
                id: "2".into(),
                board_id: "b".into(),
                title: "repin".into(),
                description: String::new(),
                url: "https://i.pinimg.com/originals/hash.jpg".into(),
                fallback_url: None,
                width: 1920,
                height: 1080,
                ..Default::default()
            },
            Pin {
                dimensions_verified: true,
                id: "3".into(),
                board_id: "b".into(),
                title: "other".into(),
                description: String::new(),
                url: "https://i.pinimg.com/736x/other.jpg".into(),
                fallback_url: None,
                width: 1920,
                height: 1080,
                ..Default::default()
            },
        ];
        assert_eq!(ranked(&lib).len(), 2);
        lib.current = Some(lib.pins[0].clone());
        assert_eq!(
            ranked(&lib)
                .into_iter()
                .map(|pin| pin.id)
                .collect::<Vec<_>>(),
            vec!["3"]
        );
    }

    #[test]
    fn full_collection_round_visits_428_assets_before_the_first_repeat() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["board".into(), crate::browser_session::SOURCE.into()];
        lib.settings.orientation = "any".into();
        lib.settings.min_width = 0;
        lib.pins = (0..428)
            .map(|index| Pin {
                dimensions_verified: index % 2 != 0,
                id: format!("{index:03}"),
                board_id: if index % 2 == 0 {
                    crate::browser_session::SOURCE.into()
                } else {
                    "board".into()
                },
                title: format!("Picture {index}"),
                description: String::new(),
                url: format!(
                    "https://i.pinimg.com/{}x/{index:03}.jpg",
                    if index % 2 == 0 { 236 } else { 736 }
                ),
                fallback_url: None,
                width: if index % 2 == 0 { 0 } else { 1920 },
                height: if index % 2 == 0 { 0 } else { 1080 },
                ..Default::default()
            })
            .collect();

        let mut visited = HashSet::new();
        let mut changes = 0;
        loop {
            assert!(
                changes <= 500,
                "rotation did not complete: {changes} unique assets"
            );
            begin_rotation_round(&mut lib);
            let candidates = foreground_rotation_candidates(&lib);
            assert!(!candidates.is_empty());
            let selected = candidates[0].clone();
            let identity = image_identity(&selected);
            if visited.contains(&identity) {
                break;
            }
            record_rotation(&mut lib, &selected);
            lib.current = Some(selected.clone());
            lib.history.push(selected.id);
            if lib.history.len() > 30 {
                lib.history.remove(0);
            }
            visited.insert(identity);
            changes += 1;
            // Exercise the same JSON round trip used by application restart
            // while the short display history has already rolled over.
            if changes % 17 == 0 {
                lib = serde_json::from_slice(&serde_json::to_vec(&lib).unwrap()).unwrap();
            }
        }

        assert_eq!(visited.len(), 428);
        assert_eq!(changes, 428);
    }

    #[test]
    fn reimport_updates_original_metadata_without_combining_decoded_variants_or_cycle() {
        let existing = Pin {
            dimensions_verified: true,
            id: "42".into(),
            board_id: "b".into(),
            title: "old title".into(),
            description: String::new(),
            url: "https://i.pinimg.com/736x/hash.jpg".into(),
            source_url: None,
            fallback_url: Some("https://i.pinimg.com/236x/hash.jpg".into()),
            original_url_exact: false,
            width: 3000,
            height: 2000,
            thumbnail_width: 736,
            thumbnail_height: 491,
            max_width: 3000,
            max_height: 2000,
            max_dimensions_source: DimensionSource::PinterestOriginal,
            max_dimensions_url: Some("https://i.pinimg.com/originals/hash.jpg".into()),
            dimensions_source: DimensionSource::Decoded,
            dimensions_url: Some("https://i.pinimg.com/736x/hash.jpg".into()),
        };
        let incoming = Pin {
            dimensions_verified: true,
            id: "42".into(),
            board_id: "b".into(),
            title: "new title".into(),
            description: "new description".into(),
            url: "https://i.pinimg.com/originals/hash.jpg".into(),
            source_url: None,
            fallback_url: None,
            original_url_exact: true,
            width: 640,
            height: 480,
            thumbnail_width: 1000,
            thumbnail_height: 667,
            max_width: 4000,
            max_height: 2667,
            max_dimensions_source: DimensionSource::PinterestOriginal,
            max_dimensions_url: Some("https://i.pinimg.com/originals/hash.jpg".into()),
            dimensions_source: DimensionSource::Thumbnail,
            dimensions_url: Some("https://i.pinimg.com/originals/hash.jpg".into()),
        };
        let merged = merge_imported_pin(&existing, incoming);

        assert!(!merged.dimensions_verified);
        assert_eq!((merged.width, merged.height), (0, 0));
        assert_eq!(
            (merged.thumbnail_width, merged.thumbnail_height),
            (1000, 667)
        );
        assert_eq!((merged.max_width, merged.max_height), (4000, 2667));
        assert_eq!(merged.dimensions_source, DimensionSource::Unknown);
        assert_eq!(
            merged.fallback_url.as_deref(),
            Some("https://i.pinimg.com/236x/hash.jpg")
        );

        let mut library = Library::default();
        library.settings.board_ids = vec!["b".into()];
        library.pins = vec![existing.clone(), pin("43", "other")];
        record_rotation(&mut library, &existing);
        let other = library.pins[1].clone();
        record_rotation_attempt(&mut library, &other);
        let seen = library.rotation_seen.clone();
        let attempted = library.rotation_attempted.clone();
        library.pins[0] = merged;
        assert_eq!(library.rotation_seen, seen);
        assert_eq!(library.rotation_attempted, attempted);
    }

    #[test]
    fn importing_new_assets_keeps_the_existing_rotation_cursor() {
        let mut lib = Library::default();
        lib.settings.board_ids = vec!["b".into()];
        lib.settings.orientation = "any".into();
        lib.settings.min_width = 0;
        lib.pins = (0..4)
            .map(|index| pin(&index.to_string(), "asset"))
            .collect();
        for index in 0..2 {
            let selected = lib.pins[index].clone();
            record_rotation(&mut lib, &selected);
        }
        let previous = lib.rotation_seen.clone();
        lib.pins.extend(
            (4..6)
                .map(|index| pin(&index.to_string(), "new asset"))
                .collect::<Vec<_>>(),
        );

        let candidates = foreground_rotation_candidates(&lib);
        assert_eq!(
            candidates
                .iter()
                .map(|pin| pin.id.as_str())
                .collect::<Vec<_>>(),
            vec!["2", "3", "4", "5", "0", "1"]
        );
        assert_eq!(lib.rotation_seen, previous);
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    #[test]
    fn validates_settings_before_scheduling() {
        let mut s = Settings::default();
        s.interval_minutes = 0;
        assert!(s.validate().is_err());
        s.interval_minutes = u64::MAX;
        assert!(s.validate().is_err());
        s.interval_minutes = 60;
        s.active_end = 24;
        assert!(s.validate().is_err());
    }
    #[test]
    fn unicode_keywords_work() {
        assert_eq!(
            words("и на Лес, море! FOREST"),
            vec!["лес", "море", "forest"]
        );
    }
    #[test]
    fn daytime_bounds_are_half_open() {
        let s = Settings::default();
        assert!(s.active(8));
        assert!(s.active(22));
        assert!(!s.active(23));
        assert!(!s.active(7));
    }
}

#[cfg(test)]
mod browser_ranking_tests {
    use super::*;
    #[test]
    fn eight_unverified_browser_pins_remain_foreground_candidates() {
        let mut library = Library::default();
        library.settings.board_ids = vec![crate::browser_session::SOURCE.into()];
        library.pins = (0..8)
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
            .collect();

        assert_eq!(rotation_candidates(&library).len(), 8);
    }

    #[test]
    fn unknown_browser_dimensions_require_download_but_known_small_images_are_filtered() {
        let mut l = Library::default();
        l.settings.board_ids = vec![crate::browser_session::SOURCE.into()];
        l.pins.push(Pin {
            dimensions_verified: false,
            id: "123".into(),
            board_id: crate::browser_session::SOURCE.into(),
            title: "Forest".into(),
            description: String::new(),
            url: String::new(),
            fallback_url: None,
            width: 0,
            height: 0,
            ..Default::default()
        });
        assert_eq!(ranked(&l).len(), 1);
        l.pins[0].width = 736;
        l.pins[0].height = 400;
        assert_eq!(ranked(&l).len(), 1); // Thumbnail metadata cannot reject the original.
        l.pins[0].dimensions_verified = true;
        assert!(ranked(&l).is_empty());
        l.pins[0].width = 2000;
        l.pins[0].height = 1000;
        assert_eq!(ranked(&l).len(), 1);
    }

    #[test]
    fn original_metadata_filters_known_small_images_before_foreground_downloads() {
        let mut library = Library::default();
        library.settings.board_ids = vec![crate::browser_session::SOURCE.into()];
        library.settings.min_width = 1920;
        library.settings.orientation = "landscape".into();
        library.pins = vec![
            Pin {
                dimensions_verified: false,
                id: "small".into(),
                board_id: crate::browser_session::SOURCE.into(),
                title: "Small original".into(),
                description: String::new(),
                url: "https://i.pinimg.com/originals/small.jpg".into(),
                fallback_url: None,
                width: 0,
                height: 0,
                max_width: 1200,
                max_height: 800,
                max_dimensions_source: DimensionSource::PinterestOriginal,
                max_dimensions_url: Some("https://i.pinimg.com/originals/small.jpg".into()),
                ..Default::default()
            },
            Pin {
                dimensions_verified: false,
                id: "large".into(),
                board_id: crate::browser_session::SOURCE.into(),
                title: "Large original".into(),
                description: String::new(),
                url: "https://i.pinimg.com/originals/large.jpg".into(),
                fallback_url: None,
                width: 0,
                height: 0,
                max_width: 2560,
                max_height: 1440,
                max_dimensions_source: DimensionSource::PinterestOriginal,
                max_dimensions_url: Some("https://i.pinimg.com/originals/large.jpg".into()),
                ..Default::default()
            },
        ];
        assert_eq!(
            foreground_rotation_candidates(&library)
                .into_iter()
                .map(|pin| pin.id)
                .collect::<Vec<_>>(),
            vec!["large"]
        );
    }

    #[test]
    fn cached_feed_thumbnail_without_source_is_eligible_for_original_recovery() {
        let mut library = Library::default();
        library.settings.board_ids = vec![crate::browser_session::SOURCE.into()];
        library.settings.min_width = 1920;
        library.settings.orientation = "landscape".into();
        library.pins = vec![Pin {
            id: "000424242424".into(), board_id: crate::browser_session::SOURCE.into(),
            url: "https://i.pinimg.com/236x/old-import.jpg".into(),
            dimensions_verified: true, width: 236, height: 354,
            dimensions_source: DimensionSource::Decoded,
            dimensions_url: Some("https://i.pinimg.com/236x/old-import.jpg".into()),
            ..Default::default()
        }];
        assert_eq!(ranked(&library).len(), 1);
        // Once the actual original was tried, final decoded pixels govern
        // the filter again, even when the image really is too small.
        library.pins[0].original_url_exact = true;
        assert!(ranked(&library).is_empty());
    }

    #[test]
    fn source_backed_pin_with_small_pinterest_fallback_stays_candidate_for_upgrade() {
        let mut library = Library::default();
        library.settings.board_ids = vec![crate::browser_session::SOURCE.into()];
        library.settings.min_width = 1920;
        library.settings.orientation = "landscape".into();
        library.pins = vec![Pin {
            dimensions_verified: true,
            id: "source-upgrade".into(),
            board_id: crate::browser_session::SOURCE.into(),
            title: "Source upgrade".into(),
            url: "https://i.pinimg.com/736x/source-upgrade.jpg".into(),
            source_url: Some("https://wallhaven.cc/w/zpvp3g".into()),
            width: 736,
            height: 414,
            dimensions_source: DimensionSource::Decoded,
            dimensions_url: Some("https://i.pinimg.com/736x/source-upgrade.jpg".into()),
            ..Default::default()
        }];

        // The last Pinterest fallback is too small, but the exact source page
        // still gives the downloader one bounded chance to find a larger file.
        assert_eq!(ranked(&library).len(), 1);
    }

    #[test]
    fn import_refresh_preserves_confirmed_quality_for_same_source_only() {
        let existing = Pin {
            dimensions_verified: true,
            id: "123".into(),
            board_id: crate::browser_session::SOURCE.into(),
            title: "Old title".into(),
            description: String::new(),
            url: "https://i.pinimg.com/736x/12/3.jpg".into(),
            fallback_url: None,
            width: 2400,
            height: 1350,
            max_width: 2400,
            max_height: 1350,
            max_dimensions_source: DimensionSource::PinterestOriginal,
            max_dimensions_url: Some("https://i.pinimg.com/originals/12/3.jpg".into()),
            dimensions_source: DimensionSource::Decoded,
            ..Default::default()
        };
        let refreshed = Pin {
            id: "123".into(),
            board_id: crate::browser_session::SOURCE.into(),
            title: "New title".into(),
            url: "https://i.pinimg.com/736x/12/3.jpg".into(),
            ..Default::default()
        };
        let merged = merge_imported_pin(&existing, refreshed);
        assert!(merged.dimensions_verified);
        assert_eq!((merged.width, merged.height), (2400, 1350));
        assert_eq!((merged.max_width, merged.max_height), (2400, 1350));
        assert_eq!(merged.title, "New title");

        let exact_existing = Pin {
            dimensions_verified: true,
            original_url_exact: true,
            id: "exact".into(),
            board_id: crate::browser_session::SOURCE.into(),
            url: "https://i.pinimg.com/originals/12/exact.png".into(),
            width: 2400,
            height: 1350,
            dimensions_source: DimensionSource::Decoded,
            dimensions_url: Some("https://i.pinimg.com/originals/12/exact.png".into()),
            ..Default::default()
        };
        let thumbnail_observation = Pin {
            id: "exact".into(),
            board_id: crate::browser_session::SOURCE.into(),
            url: "https://i.pinimg.com/736x/12/exact.jpg".into(),
            ..Default::default()
        };
        let merged_exact = merge_imported_pin(&exact_existing, thumbnail_observation);
        assert!(merged_exact.original_url_exact);
        assert_eq!(merged_exact.url, exact_existing.url);
        assert_eq!(
            merged_exact.fallback_url.as_deref(),
            Some("https://i.pinimg.com/736x/12/exact.jpg")
        );
        assert!(merged_exact.dimensions_verified);
        assert_eq!((merged_exact.width, merged_exact.height), (2400, 1350));

        let changed = Pin {
            id: "123".into(),
            board_id: crate::browser_session::SOURCE.into(),
            url: "https://i.pinimg.com/originals/12/other.jpg".into(),
            ..Default::default()
        };
        let changed = merge_imported_pin(&existing, changed);
        assert!(!changed.dimensions_verified);
        assert_eq!((changed.width, changed.height), (0, 0));
        assert_eq!((changed.max_width, changed.max_height), (0, 0));
    }

    #[test]
    fn import_refresh_preserves_source_link_and_new_source_invalidates_decoded_pixels() {
        let existing = Pin {
            dimensions_verified: true,
            id: "source-merge".into(),
            board_id: crate::browser_session::SOURCE.into(),
            url: "https://i.pinimg.com/736x/source-merge.jpg".into(),
            source_url: Some("https://wallhaven.cc/w/zpvp3g".into()),
            width: 2400,
            height: 1350,
            dimensions_source: DimensionSource::Decoded,
            dimensions_url: Some("https://w.wallhaven.cc/full/zp/wallhaven-zpvp3g.png".into()),
            ..Default::default()
        };
        let refreshed = Pin {
            id: existing.id.clone(),
            board_id: existing.board_id.clone(),
            url: existing.url.clone(),
            ..Default::default()
        };
        let merged = merge_imported_pin(&existing, refreshed);
        assert_eq!(merged.source_url, existing.source_url);
        assert!(merged.dimensions_verified);
        assert_eq!(merged.dimensions_url, existing.dimensions_url);

        let changed_source = Pin {
            id: existing.id.clone(),
            board_id: existing.board_id.clone(),
            url: existing.url.clone(),
            source_url: Some("https://www.pixelstalk.net/download/forest/".into()),
            ..Default::default()
        };
        let merged = merge_imported_pin(&existing, changed_source);
        assert_eq!(
            merged.source_url.as_deref(),
            Some("https://www.pixelstalk.net/download/forest/")
        );
        assert!(!merged.dimensions_verified);
        assert_eq!(merged.dimensions_url, None);
    }

    #[test]
    fn canonicalize_keeps_external_decoded_provenance_and_clears_it_for_a_new_source() {
        let mut library = Library::default();
        library.pins.push(Pin {
            dimensions_verified: true,
            id: "external-provenance".into(),
            board_id: crate::browser_session::SOURCE.into(),
            url: "https://i.pinimg.com/736x/external-provenance.jpg".into(),
            source_url: Some("https://wallhaven.cc/w/zpvp3g".into()),
            width: 3840,
            height: 2160,
            dimensions_source: DimensionSource::Decoded,
            dimensions_url: Some("https://w.wallhaven.cc/full/zp/wallhaven-zpvp3g.png".into()),
            ..Default::default()
        });
        assert!(!canonicalize_dimensions(&mut library));
        assert!(library.pins[0].dimensions_verified);
        assert_eq!((library.pins[0].width, library.pins[0].height), (3840, 2160));

        let refreshed = Pin {
            id: library.pins[0].id.clone(),
            board_id: library.pins[0].board_id.clone(),
            url: library.pins[0].url.clone(),
            source_url: Some("https://www.pixelstalk.net/download/other/".into()),
            ..Default::default()
        };
        let prior = library.pins[0].clone();
        library.pins[0] = merge_imported_pin(&prior, refreshed);
        assert!(!library.pins[0].dimensions_verified);
        assert_eq!(library.pins[0].dimensions_url, None);
        assert!(!canonicalize_dimensions(&mut library));
    }

    #[test]
    fn changed_filters_recover_an_exhausted_round_without_two_picture_loop() {
        let mut library = Library::default();
        library.settings.board_ids = vec!["b".into()];
        library.settings.orientation = "landscape".into();
        library.settings.min_width = 1280;
        library.pins = (0..14)
            .map(|index| Pin {
                dimensions_verified: true,
                id: format!("{index:02}"),
                board_id: "b".into(),
                width: 3840,
                height: 2160,
                ..Default::default()
            })
            .collect();
        // Persisted progress belongs to the previous filters. All candidates
        // have been inspected, including some unsuccessful downloads.
        for pin in library.pins.clone() {
            if pin.id == "00" || pin.id == "01" {
                record_rotation(&mut library, &pin);
            } else {
                record_rotation_attempt(&mut library, &pin);
            }
        }
        library.current = Some(library.pins[1].clone());
        library.settings.orientation = "any".into();
        library.settings.min_width = 1600;

        // Successful preparation is simulated; candidate selection and state
        // persistence are the same as successive manual wallpaper changes.
        for _ in 0..3 {
            let mut visited = HashSet::new();
            for _ in 0..14 {
                library = serde_json::from_slice(&serde_json::to_vec(&library).unwrap()).unwrap();
                begin_rotation_round(&mut library);
                let selected = foreground_rotation_candidates(&library)[0].clone();
                assert!(
                    visited.insert(selected.id.clone()),
                    "repeated {} before completing the round",
                    selected.id
                );
                record_rotation(&mut library, &selected);
                library.current = Some(selected);
            }
            assert_eq!(visited.len(), 14);
        }
    }

    #[test]
    fn settings_changes_do_not_reset_a_partial_rotation_round() {
        let mut library = Library::default();
        library.settings.board_ids = vec!["b".into()];
        library.settings.orientation = "any".into();
        library.settings.min_width = 0;
        library.pins = (0..4)
            .map(|index| Pin {
                dimensions_verified: true,
                id: index.to_string(),
                board_id: "b".into(),
                title: if index == 0 { "forest" } else { "ocean" }.into(),
                width: 1920,
                height: 1080,
                ..Default::default()
            })
            .collect();
        let first = library.pins[0].clone();
        record_rotation(&mut library, &first);
        let before = library.rotation_seen.clone();

        // Changing keyword priority must keep the untouched tail of the round.
        library.settings.keywords = "forest".into();
        assert!(!begin_rotation_round(&mut library));
        assert_eq!(library.rotation_seen, before);

        // Restoring the prior filter exposes the untouched tail in order.
        library.settings.keywords.clear();
        let mut restored: Library =
            serde_json::from_slice(&serde_json::to_vec(&library).unwrap()).unwrap();
        assert!(!begin_rotation_round(&mut restored));
        assert_eq!(
            foreground_rotation_candidates(&restored)
                .into_iter()
                .map(|candidate| candidate.id)
                .collect::<Vec<_>>(),
            vec!["1", "2", "3", "0"]
        );
    }

    #[test]
    fn legacy_pin_defaults_keep_unverified_metadata_unknown() {
        let legacy = serde_json::json!({
            "dimensions_verified": false,
            "id": "old",
            "board_id": crate::browser_session::SOURCE,
            "title": "Old",
            "description": "",
            "url": "https://i.pinimg.com/736x/old.jpg",
            "width": 736,
            "height": 1104
        });
        let pin: Pin = serde_json::from_value(legacy).unwrap();
        assert_eq!(filter_dimensions(&pin), None);
        assert_eq!(pin.max_dimensions_source, DimensionSource::Unknown);
        assert_eq!(pin.thumbnail_width, 0);
    }

    #[test]
    fn canonicalize_clears_legacy_page_dimensions_from_pins_and_current() {
        let stale = Pin {
            dimensions_verified: false,
            id: "old".into(),
            board_id: crate::browser_session::SOURCE.into(),
            title: "Old".into(),
            url: "https://i.pinimg.com/originals/old.jpg".into(),
            width: 2400,
            height: 1600,
            thumbnail_width: 736,
            thumbnail_height: 491,
            dimensions_source: DimensionSource::Thumbnail,
            ..Default::default()
        };
        let mut library = Library {
            pins: vec![stale.clone()],
            current: Some(stale),
            ..Default::default()
        };

        assert!(canonicalize_dimensions(&mut library));
        for pin in library.pins.iter().chain(library.current.iter()) {
            assert!(!pin.dimensions_verified);
            assert_eq!((pin.width, pin.height), (0, 0));
            assert_eq!(pin.dimensions_source, DimensionSource::Unknown);
            assert_eq!((pin.thumbnail_width, pin.thumbnail_height), (736, 491));
        }
    }

    #[test]
    fn canonicalize_marks_verified_legacy_pairs_as_decoded_and_rejects_mixed_max_metadata() {
        let mut library = Library {
            pins: vec![Pin {
                dimensions_verified: true,
                id: "old".into(),
                board_id: crate::browser_session::SOURCE.into(),
                url: "https://i.pinimg.com/736x/old.jpg".into(),
                width: 2400,
                height: 1600,
                max_width: 4000,
                max_height: 3000,
                max_dimensions_source: DimensionSource::PinterestOriginal,
                max_dimensions_url: Some("https://i.pinimg.com/originals/other.jpg".into()),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert!(canonicalize_dimensions(&mut library));
        let pin = &library.pins[0];
        assert_eq!(pin.dimensions_source, DimensionSource::Decoded);
        assert_eq!((pin.width, pin.height), (2400, 1600));
        assert_eq!((pin.max_width, pin.max_height), (0, 0));
        assert_eq!(pin.max_dimensions_source, DimensionSource::Unknown);
        assert!(pin.max_dimensions_url.is_none());
    }
}
