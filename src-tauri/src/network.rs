use crate::model::Pin;
use reqwest::blocking::Client;
use std::{
    collections::HashMap,
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use url::Url;
const MAX_PIN_PAGE: u64 = 8 * 1024 * 1024;
const MAX_SOURCE_PAGE: u64 = 2 * 1024 * 1024;
const MAX_SOURCE_REDIRECTS: usize = 3;
const MAX_SOURCE_RESOLUTION_PAGES: usize = 4;
const MAX_SOURCE_GALLERY_CANDIDATES: usize = 4;
const MAX_SOURCE_GALLERY_PREVIEWS: usize = 3;
const MAX_SOURCE_MATCH_IMAGE: u64 = 2 * 1024 * 1024;
const SOURCE_RESULT_TTL: Duration = Duration::from_secs(300);
const SOURCE_MATCH_THRESHOLD: f32 = 0.18;
const SOURCE_MATCH_MARGIN: f32 = 0.04;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedPinImage {
    pub primary: String,
    pub source_url: Option<String>,
    pub fallback: Option<String>,
    pub primary_is_original: bool,
    pub thumbnail_width: u32,
    pub thumbnail_height: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_dimensions_url: Option<String>,
}
pub fn client() -> Result<Client, String> {
    static CLIENT: std::sync::OnceLock<Result<Client, String>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("Pinpaper/0.1")
                .build()
                .map_err(|_| "Cannot initialize HTTP client".into())
        })
        .clone()
}

/// Build a source client after resolving the exact public address that will be
/// used for the request. Rebuilding this small client for each page/image
/// request lets redirects be checked and pinned independently, preventing a
/// public hostname from being reused after DNS changes to a private address.
pub fn source_client_for_url(source: &str) -> Result<Client, String> {
    let url = Url::parse(source).map_err(|_| "invalid source URL".to_owned())?;
    if !valid_source_url_parsed(&url) {
        return Err("invalid source URL".into());
    }
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(12))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Pinpaper/0.1");
    let address = public_source_address(&url)
        .ok_or_else(|| "source host did not resolve to a public address".to_owned())?;
    if let Some(url::Host::Domain(host)) = url.host() {
        builder = builder.resolve(host, address);
    }
    builder
        .build()
        .map_err(|_| "Cannot initialize source HTTP client".into())
}

/// Validate a Pinterest outbound link before it is persisted or fetched.
/// Sources are intentionally broader than the Pinterest CDN, but still need
/// HTTPS, no credentials, the default TLS port, and an address that is not a
/// local/private literal or common local-only hostname.
pub fn valid_source_url(source: &str) -> bool {
    if source.len() >= 4096 {
        return false;
    }
    Url::parse(source)
        .map(|url| valid_source_url_parsed(&url))
        .unwrap_or(false)
}

fn valid_source_url_parsed(url: &Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return false;
    }
    let Some(host) = url.host() else {
        return false;
    };
    match host {
        url::Host::Ipv4(address) => return public_ip(IpAddr::V4(address)),
        url::Host::Ipv6(address) => return public_ip(IpAddr::V6(address)),
        url::Host::Domain(host) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            if host == "pinterest.com"
                || host.ends_with(".pinterest.com")
                || host == "pinimg.com"
                || host.ends_with(".pinimg.com")
                || host == "localhost"
                || host.ends_with(".localhost")
                || host.ends_with(".local")
                || host.ends_with(".internal")
                || host.ends_with(".home.arpa")
            {
                return false;
            }
            true
        }
    }
}

fn public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            !address.is_private()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_broadcast()
        }
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(mapped));
            }
            !address.is_loopback()
                && !address.is_unique_local()
                && !address.is_unicast_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
        }
    }
}

/// Resolve a source host immediately before a request. All returned records
/// must be public; mixed public/private DNS answers are rejected so a source
/// cannot choose a private record on a later connection attempt.
fn public_source_address(url: &Url) -> Option<SocketAddr> {
    let port = url.port_or_known_default()?;
    match url.host()? {
        url::Host::Ipv4(address) => public_ip(IpAddr::V4(address))
            .then_some(SocketAddr::new(IpAddr::V4(address), port)),
        url::Host::Ipv6(address) => public_ip(IpAddr::V6(address))
            .then_some(SocketAddr::new(IpAddr::V6(address), port)),
        url::Host::Domain(host) => {
            let host = host.trim_end_matches('.');
            let addresses = resolve_source_addresses(host, port)?;
            (!addresses.is_empty() && addresses.iter().all(|address| public_ip(address.ip())))
                .then(|| addresses[0])
        }
    }
}

fn resolve_source_addresses(host: &str, port: u16) -> Option<Vec<SocketAddr>> {
    let host = host.to_owned();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("pinpaper-source-dns".into())
        .spawn(move || {
            let resolved = (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.collect::<Vec<_>>());
            let _ = sender.send(resolved);
        })
        .ok()?;
    receiver
        .recv_timeout(Duration::from_secs(2))
        .ok()?
        .ok()
}

#[derive(Clone)]
struct SourceResultCacheEntry {
    checked_at: Instant,
    result: Option<ResolvedExternalImage>,
}

fn source_results() -> &'static Mutex<HashMap<String, SourceResultCacheEntry>> {
    static RESULTS: OnceLock<Mutex<HashMap<String, SourceResultCacheEntry>>> = OnceLock::new();
    RESULTS.get_or_init(Default::default)
}

fn external_cache_key(pin: &Pin) -> Option<String> {
    let source = pin.source_url.as_deref()?.trim();
    let asset = asset_identity(&pin.url).unwrap_or_else(|| pin.url.clone());
    Some(format!("{source}\0{asset}"))
}

/// A failed/ambiguous external lookup is briefly negative-cached. This keeps
/// a source outage from turning every wallpaper change into another page walk
/// while preserving a later retry when the source may recover.
pub fn external_source_backoff(pin: &Pin) -> bool {
    let Some(key) = external_cache_key(pin) else { return false };
    let mut results = source_results().lock().unwrap();
    results.retain(|_, entry| entry.checked_at.elapsed() < SOURCE_RESULT_TTL);
    results
        .get(&key)
        .map(|entry| entry.result.is_none())
        .unwrap_or(false)
}

pub fn mark_external_source_unavailable(pin: &Pin) {
    let Some(key) = external_cache_key(pin) else { return };
    source_results().lock().unwrap().insert(
        key,
        SourceResultCacheEntry {
            checked_at: Instant::now(),
            result: None,
        },
    );
}

pub fn clear_external_source_cache() {
    source_results().lock().unwrap().clear();
    pin_page_attempts().lock().unwrap().clear();
}
pub fn valid_image_url(s: &str) -> bool {
    Url::parse(s)
        .map(|u| {
            u.scheme() == "https"
                && u.username().is_empty()
                && u.password().is_none()
                && u.port_or_known_default() == Some(443)
                && u.host_str()
                    .map(|h| h == "i.pinimg.com" || h.ends_with(".pinimg.com"))
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Return the Pinterest asset identity without the CDN size directory or file
/// extension. Pinterest may expose a JPG thumbnail and a PNG original for the
/// same asset, so URL equality and extension-preserving `/originals/` guesses
/// are not sufficient for matching variants.
pub fn asset_identity(source: &str) -> Option<String> {
    if !valid_image_url(source) {
        return None;
    }
    let url = Url::parse(source).ok()?;
    let mut segments = url.path_segments()?.filter(|segment| !segment.is_empty());
    let first = segments.next()?;
    let mut rest: Vec<_> = segments.collect();
    let is_size_path = first.split_once('x').is_some_and(|(width, height)| {
        !width.is_empty()
            && width.bytes().all(|byte| byte.is_ascii_digit())
            && (height.is_empty() || height.bytes().all(|byte| byte.is_ascii_digit()))
    });
    if first != "originals" && !is_size_path {
        rest.insert(0, first);
    }
    let last = rest.last_mut()?;
    if let Some((stem, _extension)) = last.rsplit_once('.') {
        *last = stem;
    }
    Some(format!("{}:{}", url.host_str()?, rest.join("/")))
}

/// Compare two observed CDN URLs only when both have a validated Pinterest
/// asset identity. Treating two invalid URLs as equal would allow a fallback
/// from an unrelated image to be used for a download.
pub fn same_asset(left: &str, right: &str) -> bool {
    matches!(
        (asset_identity(left), asset_identity(right)),
        (Some(left), Some(right)) if left == right
    )
}

pub fn is_original_url(source: &str) -> bool {
    let Ok(url) = Url::parse(source) else {
        return false;
    };
    valid_image_url(source)
        && url.path_segments().and_then(|mut segments| segments.next()) == Some("originals")
}

/// Choose a URL that the page actually exposed for download. Rewriting a
/// thumbnail's path to `/originals/` is unsafe because Pinterest can store the
/// original under a different extension or omit that size entirely.
pub fn preferred_download_url(pin: &Pin) -> String {
    if pin.original_url_exact {
        return pin.url.clone();
    }
    if pin.max_dimensions_source == crate::model::DimensionSource::PinterestOriginal
        && pin.max_width > 0
        && pin.max_height > 0
    {
        if let Some(url) = pin
            .max_dimensions_url
            .as_deref()
            .filter(|url| valid_image_url(url) && same_asset(url, &pin.url))
        {
            return url.to_owned();
        }
    }
    if is_original_url(&pin.url) {
        // This is a legacy guessed original. Use an observed fallback when it
        // exists; otherwise it is the only source we have and remains the last
        // bounded request made for this pin.
        return pin
            .fallback_url
            .as_deref()
            .filter(|url| valid_image_url(url) && same_asset(url, &pin.url))
            .unwrap_or(&pin.url)
            .to_owned();
    }
    pin.url.clone()
}

// Successful metadata is persisted on the Pin; unavailable pages are retried
// after a short backoff, never on every snapshot or prefetch wakeup.
fn pin_page_attempts() -> &'static Mutex<HashMap<String, Instant>> {
    static ATTEMPTS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    ATTEMPTS.get_or_init(Default::default)
}

pub fn is_resized_url(source: &str) -> bool {
    valid_image_url(source) && !is_original_url(source)
        && Url::parse(source).ok().and_then(|url| {
            url.path_segments()?.next()?.split_once('x')?.0.parse::<u32>().ok()
        }).is_some()
}

pub fn needs_pin_resolution(pin: &Pin) -> bool {
    if pin.id.is_empty() || pin.id.len() > 32 || !pin.id.bytes().all(|byte| byte.is_ascii_digit())
        || pin.original_url_exact || !(is_resized_url(&pin.url) || is_original_url(&pin.url)) {
        return false;
    }
    if pin.dimensions_verified && pin.dimensions_url.as_deref().is_some_and(is_original_url) {
        return false;
    }
    if pin.dimensions_verified && pin.source_url.is_some()
        && pin.dimensions_url.as_deref().is_some_and(valid_source_url) {
        return false;
    }
    let mut attempts = pin_page_attempts().lock().unwrap();
    attempts.retain(|_, at| at.elapsed() < Duration::from_secs(300));
    !attempts.contains_key(&pin.id)
}

fn pin_page_client() -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            let allowed = url.scheme() == "https"
                && url.username().is_empty() && url.password().is_none()
                && url.port_or_known_default() == Some(443)
                && url.host_str().is_some_and(|host|
                    host == "pinterest.com" || host.ends_with(".pinterest.com"));
            if allowed && attempt.previous().len() <= 3 { attempt.follow() }
            else { attempt.stop() }
        }))
        .user_agent("Pinpaper/0.1")
        .build().map_err(|_| "Cannot initialize Pinterest page client".into())
}

/// Read one public pin page and return only image URLs present in that page.
/// The request has no Pinterest cookies and reads one page per unresolved pin,
/// with bounded Pinterest-only redirects. Private pins can still be repaired by a
/// subsequent import from the authenticated Pinterest window.
pub fn resolve_pin_image(pin_id: &str) -> Result<ObservedPinImage, String> {
    if pin_id.is_empty() || pin_id.len() > 32 || !pin_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid Pinterest pin ID".into());
    }
    // Mark completion, not start: an in-flight prefetch must not convince a
    // simultaneous foreground request that a 236x cache is already final.
    let result = fetch_pin_image(pin_id);
    if !result.as_ref().is_ok_and(|source| source.primary_is_original) {
        pin_page_attempts().lock().unwrap().insert(pin_id.to_owned(), Instant::now());
    }
    result
}

fn fetch_pin_image(pin_id: &str) -> Result<ObservedPinImage, String> {
    let page = format!("https://www.pinterest.com/pin/{pin_id}/");
    let response = pin_page_client()?
        .get(page)
        .send()
        .map_err(|error| format!("Pinterest pin page could not be reached: {error:?}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Pinterest pin page returned HTTP {}",
            response.status()
        ));
    }
    if response.content_length().unwrap_or(0) > MAX_PIN_PAGE {
        return Err("Pinterest pin page is too large to inspect".into());
    }
    let mut body = Vec::new();
    response
        .take(MAX_PIN_PAGE + 1)
        .read_to_end(&mut body)
        .map_err(|_| "Pinterest pin page could not be read".to_owned())?;
    if body.len() as u64 > MAX_PIN_PAGE {
        return Err("Pinterest pin page is too large to inspect".into());
    }
    observed_pin_image(&String::from_utf8_lossy(&body), pin_id)
        .ok_or_else(|| "Pinterest pin page did not expose an image URL".into())
}

fn observed_pin_image(html: &str, pin_id: &str) -> Option<ObservedPinImage> {
    if !html.contains(pin_id) {
        return None;
    }
    let fallback = meta_content(html, "og:image").filter(|url| valid_image_url(url));
    let (width, height) = (
        meta_dimension(html, "og:image:width"),
        meta_dimension(html, "og:image:height"),
    );
    let fallback = fallback?;
    let asset = asset_identity(&fallback)?;
    let original = observed_originals(html)
        .into_iter()
        .find(|image| asset_identity(&image.url).as_deref() == Some(asset.as_str()));
    let (max_width, max_height, max_dimensions_url) = original
        .as_ref()
        .filter(|image| image.width > 0 && image.height > 0)
        .map(|image| (image.width, image.height, Some(image.url.clone())))
        .unwrap_or((0, 0, None));
    let primary_is_original = original.is_some();
    Some(ObservedPinImage {
        source_url: observed_pin_source(html, pin_id),
        primary: original
            .as_ref()
            .map(|image| image.url.clone())
            .unwrap_or_else(|| fallback.clone()),
        fallback: original.and_then(|image| (image.url != fallback).then_some(fallback)),
        primary_is_original,
        thumbnail_width: width,
        thumbnail_height: height,
        max_width,
        max_height,
        max_dimensions_url,
    })
}

// Only read the matching pin's own outbound field from declarative JSON.
// Relay registration calls are never evaluated as JavaScript.
fn observed_pin_source(html: &str, pin_id: &str) -> Option<String> {
    fn visit(value: &serde_json::Value, id: &str, budget: &mut usize) -> Option<String> {
        if *budget == 0 { return None; }
        *budget -= 1;
        match value {
            serde_json::Value::Object(object) => {
                if object.get("entityId").or_else(|| object.get("id"))
                    .and_then(|value| value.as_str()) == Some(id) {
                    if let Some(link) = object.get("link").and_then(|value| value.as_str())
                        .filter(|link| valid_source_url(link)) {
                        return Some(link.to_owned());
                    }
                }
                object.values().find_map(|value| visit(value, id, budget))
            }
            serde_json::Value::Array(values) => values.iter().find_map(|value| visit(value, id, budget)),
            _ => None,
        }
    }
    let mut budget = 40000;
    for block in html_blocks(html, "script") {
        let Some((tag, rest)) = block.split_once('>') else { continue };
        let Some(end) = rest.rfind("</") else { continue };
        let mut payload = rest[..end].trim();
        if let Some(arguments) = payload.strip_prefix("window.__PWS_RELAY_REGISTER_COMPLETED_REQUEST__(") {
            let mut key = serde_json::Deserializer::from_str(arguments).into_iter::<String>();
            if !matches!(key.next(), Some(Ok(_))) { continue; }
            let Some(json) = arguments[key.byte_offset()..].trim_start().strip_prefix(',') else { continue };
            payload = json.trim().trim_end_matches(';').trim_end().strip_suffix(')')?.trim();
        } else if !tag.contains("application/json") { continue; }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
            if let Some(source) = visit(&value, pin_id, &mut budget) { return Some(source); }
        }
    }
    None
}

fn meta_dimension(html: &str, property: &str) -> u32 {
    meta_content(html, property)
        .and_then(|value| value.parse().ok())
        .filter(|value: &u32| *value > 0 && *value <= 16384)
        .unwrap_or(0)
}

fn meta_content(html: &str, wanted: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let wanted = wanted.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find("<meta") {
        let start = cursor + offset;
        let after_name = start + "<meta".len();
        if lower
            .as_bytes()
            .get(after_name)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
        {
            cursor = after_name;
            continue;
        }
        let Some(end_offset) = lower[start..].find('>') else {
            break;
        };
        let end = start + end_offset + 1;
        let tag = &html[start..end];
        let property = html_attribute(tag, "property")
            .or_else(|| html_attribute(tag, "name"))
            .map(|value| value.to_ascii_lowercase());
        if property.as_deref() == Some(wanted.as_str()) {
            return html_attribute(tag, "content");
        }
        cursor = end;
    }
    None
}

fn html_attribute(tag: &str, wanted: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let wanted = wanted.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut cursor = 0;
    while cursor + wanted.len() <= bytes.len() {
        let Some(offset) = lower[cursor..].find(&wanted) else {
            break;
        };
        let start = cursor + offset;
        let boundary_before = start == 0 || bytes[start - 1].is_ascii_whitespace();
        let after = start + wanted.len();
        let boundary_after = bytes
            .get(after)
            .map(|byte| byte.is_ascii_whitespace() || *byte == b'=')
            .unwrap_or(true);
        if !boundary_before || !boundary_after {
            cursor = after;
            continue;
        }
        let mut value_start = after;
        while bytes
            .get(value_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        if bytes.get(value_start) != Some(&b'=') {
            cursor = after;
            continue;
        }
        value_start += 1;
        while bytes
            .get(value_start)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            value_start += 1;
        }
        let quote = *bytes.get(value_start)?;
        if quote == b'\'' || quote == b'"' {
            let content_start = value_start + 1;
            let content_end = lower[content_start..].find(quote as char)? + content_start;
            return Some(tag[content_start..content_end].to_owned());
        }
        let content_end = lower[value_start..]
            .find(|ch: char| ch.is_ascii_whitespace() || ch == '>')
            .map(|end| value_start + end)
            .unwrap_or(tag.len());
        return Some(tag[value_start..content_end].to_owned());
    }
    None
}

struct ObservedImage {
    url: String,
    width: u32,
    height: u32,
}

fn observed_originals(html: &str) -> Vec<ObservedImage> {
    let mut urls = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find("\"images_orig\"") {
        let start = cursor + offset;
        let mut end = (start + 4096).min(html.len());
        while !html.is_char_boundary(end) { end -= 1; }
        let snippet = &html[start..end];
        let object = json_object(snippet).unwrap_or(snippet);
        if let Some(url) = pinimg_url(object) {
            if !urls.iter().any(|image: &ObservedImage| image.url == url) {
                let width = json_dimension(object, "width");
                let height = json_dimension(object, "height");
                urls.push(ObservedImage { url, width, height });
            }
        }
        cursor = start + "\"images_orig\"".len();
    }
    urls
}

/// Return the first bounded JSON object after an images_orig marker. Keeping
/// the width/height lookup inside that object prevents a nearby thumbnail
/// record from being mistaken for original metadata.
fn json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return text.get(start..=start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Read a positive JSON number only from the bounded images_orig snippet. If
/// Pinterest does not attach dimensions to that exact object, leave them
/// unknown rather than borrowing the nearby og:image thumbnail dimensions.
fn json_dimension(text: &str, wanted: &str) -> u32 {
    let marker = format!("\"{wanted}\"");
    let Some(offset) = text.find(&marker) else {
        return 0;
    };
    let rest = &text[offset + marker.len()..];
    let Some(colon) = rest.find(':') else {
        return 0;
    };
    let digits: String = rest[colon + 1..]
        .chars()
        .skip_while(|ch| ch.is_ascii_whitespace())
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0 && *value <= 16384)
        .unwrap_or(0)
}

fn pinimg_url(text: &str) -> Option<String> {
    let marker = "https://i.pinimg.com/";
    let start = text.find(marker)?;
    let rest = &text[start..];
    let end = rest
        .find(|ch: char| ch == '"' || ch == '\'' || ch == '<' || ch.is_ascii_whitespace())
        .unwrap_or(rest.len());
    let url = rest[..end].replace("\\/", "/");
    valid_image_url(&url).then_some(url)
}

/// An image URL discovered from the outbound source page.  The dimensions are
/// only page hints; the decoded bytes written to the cache remain authoritative
/// for filtering and provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedExternalImage {
    pub page_url: String,
    pub image_url: String,
    pub width: u32,
    pub height: u32,
}

/// Resolve one Pinterest pin's outbound page to an exact image URL.  A source
/// failure deliberately returns `None`: the caller must continue with the
/// observed Pinterest URL in the same bounded download attempt.
pub fn resolve_external_image(pin: &Pin) -> Option<ResolvedExternalImage> {
    let key = external_cache_key(pin)?;
    {
        let mut results = source_results().lock().unwrap();
        results.retain(|_, entry| entry.checked_at.elapsed() < SOURCE_RESULT_TTL);
        if let Some(entry) = results.get(&key) {
            return entry.result.clone();
        }
    }
    let result = resolve_external_image_uncached(pin);
    source_results().lock().unwrap().insert(
        key,
        SourceResultCacheEntry {
            checked_at: Instant::now(),
            result: result.clone(),
        },
    );
    result
}

fn resolve_external_image_uncached(pin: &Pin) -> Option<ResolvedExternalImage> {
    let source = pin.source_url.as_deref()?.trim();
    let source_url = Url::parse(source).ok()?;
    if !valid_source_url_parsed(&source_url) {
        return None;
    }
    if looks_like_image_url(&source_url) {
        return Some(ResolvedExternalImage {
            page_url: source_url.to_string(),
            image_url: source_url.to_string(),
            width: 0,
            height: 0,
        });
    }
    let (final_url, body) = fetch_source_page(&source_url).ok()?;
    let host = final_url.host_str().unwrap_or_default();
    let html = String::from_utf8_lossy(&body);
    if !is_wallpaperscraft_host(host) && !is_wallhaven_host(host) {
        let gallery = gallery_candidates(&final_url, &html);
        let gallery_families = unique_image_families(
            gallery
                .iter()
                .map(|candidate| candidate.image_url.as_str()),
        );
        if gallery.len() > 1 && (is_pixelstalk_host(host) || gallery_families.len() > 1) {
            // A page with several unrelated full images is a gallery. Match
            // the small page preview against the pinned Pinterest pixels and
            // accept only one clearly better candidate; never pick the
            // largest gallery file by filename alone.
            return resolve_gallery_by_preview(pin, &final_url, &gallery);
        }
    }
    let resolution_pages = if is_wallpaperscraft_host(host) {
        wallpaperscraft_resolution_pages(&final_url, &html)
    } else {
        Vec::new()
    };
    let mut resolved = resolve_source_document(&final_url, &body);
    // WallpapersCraft exposes every available resolution as a link on the
    // wallpaper page. Follow only a few of those observed same-wallpaper
    // links, ordered largest-first, so a pin saved at 1366x768 can still reach
    // an actually published 4K file without inventing a URL or crawling the
    // site. If a larger page cannot be fetched, the current exact file remains
    // a valid external candidate.
    // Landing pages often expose only links to those download pages. Because
    // the links were observed in the page, this remains bounded and does not
    // synthesize a `/download/...` URL from the landing-page slug.
    for resolution_page in resolution_pages
        .into_iter()
        .take(MAX_SOURCE_RESOLUTION_PAGES)
    {
        if resolution_page == source_url || resolution_page == final_url {
            continue;
        }
        let Ok((page_url, page_body)) = fetch_source_page(&resolution_page) else {
            continue;
        };
        let Some(candidate) = resolve_source_document(&page_url, &page_body) else {
            continue;
        };
        let replace = resolved
            .as_ref()
            .map(|current| {
                candidate.width.saturating_mul(candidate.height)
                    > current.width.saturating_mul(current.height)
            })
            .unwrap_or(true);
        if replace {
            resolved = Some(candidate);
        }
    }
    let resolved = resolved?;
    Some(resolved)
}

fn fetch_source_page(start: &Url) -> Result<(Url, Vec<u8>), String> {
    if !valid_source_url_parsed(start) {
        return Err("invalid source URL".into());
    }
    let mut current = start.clone();
    for redirect in 0..=MAX_SOURCE_REDIRECTS {
        let client = source_client_for_url(current.as_str())?;
        let response = client
            .get(current.clone())
            .send()
            .map_err(|_| "source page request failed")?;
        if response.status().is_redirection() {
            if redirect == MAX_SOURCE_REDIRECTS {
                return Err("source page redirected too many times".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or("source page redirect had no location")?;
            let next = current
                .join(location)
                .map_err(|_| "source page redirect was invalid")?;
            if !valid_source_url_parsed(&next) {
                return Err("source page redirect left the public HTTPS scope".into());
            }
            current = next;
            continue;
        }
        if !response.status().is_success() {
            return Err("source page returned an error".into());
        }
        if response.content_length().unwrap_or(0) > MAX_SOURCE_PAGE {
            return Err("source page is too large to inspect".into());
        }
        let mut body = Vec::new();
        response
            .take(MAX_SOURCE_PAGE + 1)
            .read_to_end(&mut body)
            .map_err(|_| "source page could not be read")?;
        if body.len() as u64 > MAX_SOURCE_PAGE {
            return Err("source page is too large to inspect".into());
        }
        return Ok((current, body));
    }
    Err("source page could not be fetched".into())
}

fn resolve_source_document(page_url: &Url, body: &[u8]) -> Option<ResolvedExternalImage> {
    let html = String::from_utf8_lossy(body);
    let host = page_url.host_str().unwrap_or_default();
    if is_wallpaperscraft_host(host) {
        return resolve_wallpaperscraft(page_url, &html);
    }
    if is_wallhaven_host(host) {
        return resolve_wallhaven(page_url, &html);
    }
    if is_pixelstalk_host(host) {
        return resolve_pixelstalk(page_url, &html);
    }
    resolve_generic_source(page_url, &html)
}

fn is_wallpaperscraft_host(host: &str) -> bool {
    host == "wallpaperscraft.com" || host.ends_with(".wallpaperscraft.com")
}

fn is_wallhaven_host(host: &str) -> bool {
    matches!(
        host,
        "wallhaven.cc" | "www.wallhaven.cc" | "w.wallhaven.cc" | "whvn.cc"
    )
}

fn is_pixelstalk_host(host: &str) -> bool {
    host == "pixelstalk.net" || host.ends_with(".pixelstalk.net")
}

fn looks_like_image_url(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn resolve_wallpaperscraft(page_url: &Url, html: &str) -> Option<ResolvedExternalImage> {
    let segments: Vec<_> = page_url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .collect();
    let slug = (segments.first() == Some(&"download"))
        .then(|| segments.get(1).copied())
        .flatten()?;
    let mut candidates = Vec::new();
    for tag in html_tags(html, "a")
        .into_iter()
        .chain(html_tags(html, "img"))
        .chain(html_tags(html, "meta"))
        .chain(html_tags(html, "link"))
    {
        let tag_name = tag.to_ascii_lowercase();
        let raw_urls = if tag_name.starts_with("<meta") {
            vec![html_attribute(tag, "content")]
        } else if tag_name.starts_with("<img") {
            vec![
                html_attribute(tag, "src"),
                html_attribute(tag, "data-src"),
                html_attribute(tag, "data-lazy-src"),
            ]
        } else {
            vec![html_attribute(tag, "href")]
        };
        for raw in raw_urls.into_iter().flatten() {
            let Some(url) = source_url_from(page_url, &raw) else {
                continue;
            };
            if url.host_str() != Some("images.wallpaperscraft.com")
                || !looks_like_image_url(&url)
                || !url
                    .path_segments()
                    .into_iter()
                    .flatten()
                    .last()
                    .is_some_and(|name| name.contains(slug))
            {
                continue;
            }
            let (width, height) = dimensions_from_text(url.path());
            if !candidates.iter().any(|candidate: &SourceCandidate| {
                candidate.url == url.to_string()
            }) {
                candidates.push(SourceCandidate {
                    url: url.to_string(),
                    width,
                    height,
                    confidence: 100,
                });
            }
        }
    }
    let best = best_source_candidate(candidates)?;
    Some(ResolvedExternalImage {
        page_url: page_url.to_string(),
        image_url: best.url,
        width: best.width,
        height: best.height,
    })
}

fn wallpaperscraft_resolution_pages(page_url: &Url, html: &str) -> Vec<Url> {
    let segments: Vec<_> = page_url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .collect();
    let Some(kind) = segments.first().copied() else {
        return Vec::new();
    };
    if kind != "download" && kind != "wallpaper" {
        return Vec::new();
    }
    let Some(slug) = segments.get(1).copied() else {
        return Vec::new();
    };
    let mut pages = Vec::new();
    for tag in html_tags(html, "a") {
        let Some(raw) = html_attribute(tag, "href") else {
            continue;
        };
        let Some(url) = source_url_from(page_url, &raw) else {
            continue;
        };
        let parts: Vec<_> = url
            .path_segments()
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect();
        if !is_wallpaperscraft_host(url.host_str().unwrap_or_default())
            || parts.first() != Some(&"download")
            || parts.get(1) != Some(&slug)
            || parts.get(2).is_none_or(|resolution| {
                dimensions_from_text(resolution) == (0, 0)
            })
        {
            continue;
        }
        if !pages.contains(&url) {
            pages.push(url);
        }
    }
    pages.sort_by_key(|url| {
        let (width, height) = dimensions_from_text(url.path());
        std::cmp::Reverse(width.saturating_mul(height))
    });
    pages
}

fn resolve_wallhaven(page_url: &Url, html: &str) -> Option<ResolvedExternalImage> {
    let mut candidates = Vec::new();
    for tag in html_tags(html, "img") {
        let is_wallpaper = html_attribute(tag, "id")
            .is_some_and(|id| id.eq_ignore_ascii_case("wallpaper"));
        let raw = html_attribute(tag, "src")
            .or_else(|| html_attribute(tag, "data-src"))
            .or_else(|| html_attribute(tag, "data-original"));
        let Some(raw) = raw else { continue };
        let Some(url) = source_url_from(page_url, &raw) else {
            continue;
        };
        let full_host = url.host_str() == Some("w.wallhaven.cc");
        let full_path = url.path().contains("/full/");
        if !(is_wallpaper || (full_host && full_path)) || !looks_like_image_url(&url) {
            continue;
        }
        let (width, height) = dimensions_from_text(url.path());
        candidates.push(SourceCandidate {
            url: url.to_string(),
            width,
            height,
            confidence: if is_wallpaper { 120 } else { 100 },
        });
    }
    // Some Wallhaven pages expose the exact image as a direct anchor even if
    // the main image is lazy-loaded. The `/full/` host/path is an explicit
    // source signal, unlike the social thumbnail in og:image.
    for tag in html_tags(html, "a") {
        let Some(raw) = html_attribute(tag, "href") else {
            continue;
        };
        let Some(url) = source_url_from(page_url, &raw) else {
            continue;
        };
        if url.host_str() == Some("w.wallhaven.cc")
            && url.path().contains("/full/")
            && looks_like_image_url(&url)
        {
            let (width, height) = dimensions_from_text(url.path());
            candidates.push(SourceCandidate {
                url: url.to_string(),
                width,
                height,
                confidence: 105,
            });
        }
    }
    let best = best_source_candidate(candidates)?;
    Some(ResolvedExternalImage {
        page_url: page_url.to_string(),
        image_url: best.url,
        width: best.width,
        height: best.height,
    })
}

fn resolve_pixelstalk(page_url: &Url, html: &str) -> Option<ResolvedExternalImage> {
    let metadata = meta_image_candidates(page_url, html);
    let links = direct_image_links(page_url, html);
    // Multi-family PixelsTalk galleries are handled by the preview matcher
    // before this function. A single-family page can safely use its strongest
    // observed URL, while a page with unrelated metadata remains ambiguous.
    let families = unique_image_families(
        links
            .iter()
            .map(|candidate| candidate.url.as_str())
            .chain(metadata.iter().map(|candidate| candidate.url.as_str())),
    );
    if families.len() > 1 {
        return None;
    }
    let candidate = best_source_candidate(metadata.into_iter().chain(links).collect())?;
    Some(ResolvedExternalImage {
        page_url: page_url.to_string(),
        image_url: candidate.url,
        width: candidate.width,
        height: candidate.height,
    })
}

#[derive(Clone, Debug)]
struct GalleryCandidate {
    image_url: String,
    preview_urls: Vec<String>,
    width: u32,
    height: u32,
    full_priority: u8,
}

/// Collect a small amount of image-to-preview structure from the source page.
/// A linked full-size image is paired with the `img`/`srcset` inside the same
/// anchor; standalone images are retained only when they are not already part
/// of a linked candidate. This is enough for common WordPress galleries while
/// avoiding a general HTML crawler or DOM dependency.
fn gallery_candidates(page_url: &Url, html: &str) -> Vec<GalleryCandidate> {
    let mut candidates = Vec::new();
    for block in html_blocks(html, "a") {
        let anchor = html_tags(block, "a").into_iter().next().unwrap_or(block);
        let linked = html_attribute(anchor, "href")
            .and_then(|raw| source_url_from(page_url, &raw))
            .filter(|url| looks_like_image_url(url));
        let full_priority = if linked.is_some() { 3 } else { 1 };
        let image_urls = html_tags(block, "img")
            .into_iter()
            .flat_map(|tag| gallery_tag_images(page_url, tag))
            .collect::<Vec<_>>();
        let Some(image_url) = linked.or_else(|| {
            image_urls
                .iter()
                .max_by_key(|candidate| source_candidate_score(candidate))
                .map(|candidate| Url::parse(&candidate.url).ok())
                .flatten()
        }) else {
            continue;
        };
        let previews = gallery_preview_urls(&image_url, &image_urls);
        push_gallery_candidate(&mut candidates, image_url, previews, full_priority);
    }

    // Some themes omit the anchor around a gallery tile and expose only an
    // image/srcset. Keep those as candidates as well, deduplicating against
    // the anchor pass above.
    for tag in html_tags(html, "img") {
        let images = gallery_tag_images(page_url, tag);
        let Some(image) = images
            .iter()
            .max_by_key(|candidate| source_candidate_score(candidate))
            .and_then(|candidate| Url::parse(&candidate.url).ok())
        else {
            continue;
        };
        let previews = gallery_preview_urls(&image, &images);
        push_gallery_candidate(&mut candidates, image, previews, 1);
    }
    candidates
}

fn push_gallery_candidate(
    candidates: &mut Vec<GalleryCandidate>,
    image_url: Url,
    previews: Vec<String>,
    full_priority: u8,
) {
    let image_url = image_url.to_string();
    let (width, height) = dimensions_from_text(&image_url);
    let family = Url::parse(&image_url).ok().map(|url| image_family(&url));
    if let Some(existing) = candidates.iter_mut().find(|candidate| {
        let same_family = family.as_deref().is_some_and(|family| {
            Url::parse(&candidate.image_url)
                .ok()
                .map(|url| image_family(&url))
                .map(|candidate_family| candidate_family == family)
                .unwrap_or(false)
        });
        candidate.image_url == image_url || same_family
    }) {
        for preview in previews {
            if !existing.preview_urls.contains(&preview) {
                existing.preview_urls.push(preview);
            }
        }
        let existing_area = existing.width.saturating_mul(existing.height);
        let candidate_area = width.saturating_mul(height);
        if full_priority > existing.full_priority
            || (full_priority == existing.full_priority && candidate_area > existing_area)
        {
            existing.image_url = image_url;
            existing.width = width;
            existing.height = height;
            existing.full_priority = full_priority;
        }
        return;
    }
    candidates.push(GalleryCandidate {
        image_url,
        preview_urls: previews,
        width,
        height,
        full_priority,
    });
}

fn gallery_preview_urls(image_url: &Url, images: &[SourceCandidate]) -> Vec<String> {
    let mut previews = Vec::new();
    for candidate in images {
        if candidate.url == image_url.as_str() || previews.contains(&candidate.url) {
            continue;
        }
        previews.push(candidate.url.clone());
    }
    if previews.is_empty() {
        previews.push(image_url.to_string());
    }
    previews.truncate(MAX_SOURCE_GALLERY_PREVIEWS);
    previews
}

fn gallery_tag_images(page_url: &Url, tag: &str) -> Vec<SourceCandidate> {
    let mut candidates = Vec::new();
    for (raw, confidence) in [
        (html_attribute(tag, "src"), 60),
        (html_attribute(tag, "data-src"), 90),
        (html_attribute(tag, "data-original"), 100),
        (html_attribute(tag, "data-lazy-src"), 90),
    ] {
        let Some(raw) = raw else { continue };
        let Some(url) = source_url_from(page_url, &raw) else {
            continue;
        };
        if !looks_like_image_url(&url) {
            continue;
        }
        let (width, height) = dimensions_from_text(url.path());
        candidates.push(SourceCandidate {
            url: url.to_string(),
            width,
            height,
            confidence,
        });
    }
    if let Some(raw) = html_attribute(tag, "srcset")
        .or_else(|| html_attribute(tag, "data-srcset"))
    {
        for item in raw.split(',') {
            let mut fields = item.split_whitespace();
            let Some(raw_url) = fields.next() else { continue };
            let Some(url) = source_url_from(page_url, raw_url) else {
                continue;
            };
            if !looks_like_image_url(&url) {
                continue;
            }
            let descriptor = fields.next().unwrap_or_default();
            let rank = descriptor
                .strip_suffix('w')
                .and_then(|value| value.parse::<u32>().ok())
                .or_else(|| {
                    descriptor
                        .strip_suffix('x')
                        .and_then(|value| value.parse::<u32>().ok())
                        .map(|value| value.saturating_mul(1000))
                })
                .unwrap_or(0);
            let (width, height) = dimensions_from_text(url.path());
            candidates.push(SourceCandidate {
                url: url.to_string(),
                width: width.max(rank),
                height,
                confidence: 80,
            });
        }
    }
    candidates
}

fn html_blocks<'a>(html: &'a str, wanted: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let opening = format!("<{wanted}");
    let closing = format!("</{wanted}");
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find(&opening) {
        let start = cursor + offset;
        let after = start + opening.len();
        if lower
            .as_bytes()
            .get(after)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>' && *byte != b'/')
        {
            cursor = after;
            continue;
        }
        let Some(open_end_offset) = lower[start..].find('>') else {
            break;
        };
        let content_start = start + open_end_offset + 1;
        let Some(close_offset) = lower[content_start..].find(&closing) else {
            break;
        };
        let close_start = content_start + close_offset;
        let Some(close_end_offset) = lower[close_start..].find('>') else {
            break;
        };
        let end = close_start + close_end_offset + 1;
        blocks.push(&html[start..end]);
        cursor = end;
    }
    blocks
}

fn resolve_gallery_by_preview(
    pin: &Pin,
    page_url: &Url,
    candidates: &[GalleryCandidate],
) -> Option<ResolvedExternalImage> {
    let preview_client = client().ok()?;
    let mut preview = None;
    for preview_url in [Some(pin.url.as_str()), pin.fallback_url.as_deref()]
        .into_iter()
        .flatten()
        .filter(|url| valid_image_url(url) && same_asset(url, &pin.url))
    {
        let Some(bytes) = read_bounded_image(&preview_client, preview_url, MAX_SOURCE_MATCH_IMAGE)
        else {
            continue;
        };
        if let Some(image) = decode_match_image(&bytes) {
            preview = Some(image);
            break;
        }
    }
    let preview = preview?;
    let preview_fingerprint = image_fingerprint(&preview)?;
    if preview_fingerprint.variance < 0.002 {
        return None;
    }

    let mut matches = Vec::new();
    for candidate in candidates.iter().take(MAX_SOURCE_GALLERY_CANDIDATES) {
        let mut best = None;
        for preview_url in candidate
            .preview_urls
            .iter()
            .take(MAX_SOURCE_GALLERY_PREVIEWS)
        {
            let Ok(candidate_client) = source_client_for_url(preview_url) else {
                continue;
            };
            let Some(bytes) = read_bounded_image(
                &candidate_client,
                preview_url,
                MAX_SOURCE_MATCH_IMAGE,
            ) else {
                continue;
            };
            let Some(image) = decode_match_image(&bytes) else {
                continue;
            };
            let Some(fingerprint) = image_fingerprint(&image) else {
                continue;
            };
            let Some(score) = fingerprint_distance(&preview_fingerprint, &fingerprint) else {
                continue;
            };
            if best.map(|current: f32| score < current).unwrap_or(true) {
                best = Some(score);
            }
        }
        if let Some(score) = best {
            matches.push((score, candidate));
        }
    }
    let candidate = choose_gallery_match(matches)?;
    Some(ResolvedExternalImage {
        page_url: page_url.to_string(),
        image_url: candidate.image_url.clone(),
        width: candidate.width,
        height: candidate.height,
    })
}

fn choose_gallery_match<'a>(
    mut matches: Vec<(f32, &'a GalleryCandidate)>,
) -> Option<&'a GalleryCandidate> {
    matches.retain(|(score, _)| *score <= SOURCE_MATCH_THRESHOLD);
    matches.sort_by(|left, right| left.0.total_cmp(&right.0));
    let (score, candidate) = matches.first().copied()?;
    if matches
        .get(1)
        .map(|(next, _)| *next - score < SOURCE_MATCH_MARGIN)
        .unwrap_or(false)
    {
        return None;
    }
    Some(candidate)
}

fn read_bounded_image(client: &Client, url: &str, max_bytes: u64) -> Option<Vec<u8>> {
    let response = client.get(url).send().ok()?;
    if !response.status().is_success() || response.content_length().unwrap_or(0) > max_bytes {
        return None;
    }
    let mut bytes = Vec::new();
    response.take(max_bytes + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max_bytes).then_some(bytes)
}

fn decode_match_image(bytes: &[u8]) -> Option<image::DynamicImage> {
    use image::ImageDecoder;

    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    (image.width() > 0 && image.height() > 0).then_some(image)
}

struct ImageFingerprint {
    aspect: f32,
    variance: f32,
    pixels: Vec<f32>,
}

fn image_fingerprint(image: &image::DynamicImage) -> Option<ImageFingerprint> {
    if image.width() == 0 || image.height() == 0 {
        return None;
    }
    let resized = image
        .resize_exact(32, 32, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let mut pixels = Vec::with_capacity(32 * 32 * 3);
    let mut luminances = Vec::with_capacity(32 * 32);
    for pixel in resized.pixels() {
        let [red, green, blue] = pixel.0;
        for channel in [red, green, blue] {
            let value = f32::from(channel) / 255.0;
            pixels.push(value);
        }
        luminances.push(
            0.2126 * f32::from(red) / 255.0
                + 0.7152 * f32::from(green) / 255.0
                + 0.0722 * f32::from(blue) / 255.0,
        );
    }
    let mean = luminances.iter().sum::<f32>() / luminances.len() as f32;
    let variance = luminances
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f32>()
        / luminances.len() as f32;
    Some(ImageFingerprint {
        aspect: image.width() as f32 / image.height() as f32,
        variance,
        pixels,
    })
}

fn fingerprint_distance(left: &ImageFingerprint, right: &ImageFingerprint) -> Option<f32> {
    let aspect_delta = (left.aspect - right.aspect).abs() / left.aspect.max(right.aspect);
    if aspect_delta > 0.06 || right.variance < 0.002 {
        return None;
    }
    let mse = left
        .pixels
        .iter()
        .zip(&right.pixels)
        .map(|(left, right)| (left - right).powi(2))
        .sum::<f32>()
        / left.pixels.len() as f32;
    Some(mse.sqrt())
}

fn resolve_generic_source(page_url: &Url, html: &str) -> Option<ResolvedExternalImage> {
    let metadata = meta_image_candidates(page_url, html);
    let links = direct_image_links(page_url, html);
    let structured = structured_image_candidates(page_url, html);
    let srcsets = srcset_candidates(page_url, html);
    let families = unique_image_families(
        links
            .iter()
            .map(|candidate| candidate.url.as_str())
            .chain(metadata.iter().map(|candidate| candidate.url.as_str()))
            .chain(structured.iter().map(|candidate| candidate.url.as_str()))
            .chain(srcsets.iter().map(|candidate| candidate.url.as_str())),
    );
    if families.len() > 1 {
        return None;
    }
    let mut candidates = metadata;
    candidates.extend(links);
    candidates.extend(structured);
    // A page's `og:image` is often a small social preview while the exact
    // same image is available through a larger `srcset` entry. Keep all
    // evidence in the candidate pool and let the bounded score choose the
    // largest member of the one unambiguous image family.
    candidates.extend(srcsets);
    let candidate = best_source_candidate(candidates)?;
    Some(ResolvedExternalImage {
        page_url: page_url.to_string(),
        image_url: candidate.url,
        width: candidate.width,
        height: candidate.height,
    })
}

#[derive(Clone, Debug)]
struct SourceCandidate {
    url: String,
    width: u32,
    height: u32,
    confidence: u8,
}

fn best_source_candidate(candidates: Vec<SourceCandidate>) -> Option<SourceCandidate> {
    let mut deduplicated = Vec::new();
    for candidate in candidates {
        if let Some(index) = deduplicated
            .iter()
            .position(|existing: &SourceCandidate| existing.url == candidate.url)
        {
            let existing = &mut deduplicated[index];
            if candidate.confidence > existing.confidence
                || candidate.width.saturating_mul(candidate.height)
                    > existing.width.saturating_mul(existing.height)
            {
                *existing = candidate;
            }
        } else {
            deduplicated.push(candidate);
        }
    }
    deduplicated.into_iter().max_by(|left, right| {
        source_candidate_score(left)
            .cmp(&source_candidate_score(right))
            .then_with(|| left.confidence.cmp(&right.confidence))
            .then_with(|| left.url.cmp(&right.url))
    })
}

fn source_candidate_score(candidate: &SourceCandidate) -> u64 {
    let area = candidate.width as u64 * candidate.height as u64;
    // A srcset width descriptor is a quality signal even when the URL itself
    // has no dimensions in its filename. Squaring the width keeps it
    // comparable with a metadata width*height pair for the same image family.
    if candidate.confidence == 80 && candidate.height == 0 {
        candidate.width as u64 * candidate.width as u64
    } else {
        area
    }
}

fn meta_image_candidates(page_url: &Url, html: &str) -> Vec<SourceCandidate> {
    let mut candidates = Vec::new();
    let (meta_width, meta_height) = (
        html_meta_dimension(html, "og:image:width"),
        html_meta_dimension(html, "og:image:height"),
    );
    for tag in html_tags(html, "meta") {
        let key = html_attribute(tag, "property")
            .or_else(|| html_attribute(tag, "name"))
            .map(|value| value.to_ascii_lowercase());
        if !matches!(key.as_deref(), Some("og:image") | Some("twitter:image")) {
            continue;
        }
        let Some(raw) = html_attribute(tag, "content") else {
            continue;
        };
        let Some(url) = source_url_from(page_url, &raw) else {
            continue;
        };
        if !looks_like_image_url(&url) {
            continue;
        }
        let (width, height) = dimensions_from_text(url.path());
        candidates.push(SourceCandidate {
            url: url.to_string(),
            width: if meta_width > 0 { meta_width } else { width },
            height: if meta_height > 0 { meta_height } else { height },
            confidence: if key.as_deref() == Some("og:image") {
                110
            } else {
                90
            },
        });
    }
    candidates
}

fn direct_image_links(page_url: &Url, html: &str) -> Vec<SourceCandidate> {
    let mut candidates = Vec::new();
    for tag in html_tags(html, "a") {
        let Some(raw) = html_attribute(tag, "href") else {
            continue;
        };
        let Some(url) = source_url_from(page_url, &raw) else {
            continue;
        };
        if !looks_like_image_url(&url) {
            continue;
        }
        let (width, height) = dimensions_from_text(url.path());
        candidates.push(SourceCandidate {
            url: url.to_string(),
            width,
            height,
            confidence: 100,
        });
    }
    candidates
}

fn srcset_candidates(page_url: &Url, html: &str) -> Vec<SourceCandidate> {
    let mut candidates = Vec::new();
    for tag in html_tags(html, "img")
        .into_iter()
        .chain(html_tags(html, "source"))
    {
        let Some(raw) = html_attribute(tag, "srcset") else {
            continue;
        };
        let mut best = None;
        for item in raw.split(',') {
            let mut fields = item.split_whitespace();
            let Some(raw_url) = fields.next() else { continue };
            let descriptor = fields.next().unwrap_or_default();
            let Some(url) = source_url_from(page_url, raw_url) else {
                continue;
            };
            if !looks_like_image_url(&url) {
                continue;
            }
            let rank = descriptor
                .strip_suffix('w')
                .and_then(|value| value.parse::<u32>().ok())
                .or_else(|| {
                    descriptor
                        .strip_suffix('x')
                        .and_then(|value| value.parse::<u32>().ok())
                        .map(|value| value.saturating_mul(1000))
                })
                .unwrap_or(0);
            let (width, height) = dimensions_from_text(url.path());
            let candidate = SourceCandidate {
                url: url.to_string(),
                width: width.max(rank),
                height,
                confidence: 80,
            };
            if best
                .as_ref()
                .is_none_or(|current: &SourceCandidate| candidate.width > current.width)
            {
                best = Some(candidate);
            }
        }
        if let Some(candidate) = best {
            candidates.push(candidate);
        }
    }
    candidates
}

fn structured_image_candidates(page_url: &Url, html: &str) -> Vec<SourceCandidate> {
    let mut candidates = Vec::new();
    for tag in html_tags(html, "script") {
        if html_attribute(tag, "type")
            .as_deref()
            .is_none_or(|value| !value.eq_ignore_ascii_case("application/ld+json"))
        {
            continue;
        }
        let Some(start) = html.find(tag) else { continue };
        let body_start = start + tag.len();
        let Some(end) = html[body_start..].to_ascii_lowercase().find("</script") else {
            continue;
        };
        let json = &html[body_start..body_start + end];
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
            continue;
        };
        let mut urls = Vec::new();
        collect_structured_images(&value, false, &mut urls);
        for raw in urls {
            let Some(url) = source_url_from(page_url, &raw) else {
                continue;
            };
            if !looks_like_image_url(&url) {
                continue;
            }
            let (width, height) = dimensions_from_text(url.path());
            candidates.push(SourceCandidate {
                url: url.to_string(),
                width,
                height,
                confidence: 95,
            });
        }
    }
    candidates
}

fn collect_structured_images(
    value: &serde_json::Value,
    image_context: bool,
    urls: &mut Vec<String>,
) {
    match value {
        serde_json::Value::String(value) if image_context => urls.push(value.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_structured_images(value, image_context, urls);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                let context = matches!(
                    key.to_ascii_lowercase().as_str(),
                    "image" | "contenturl" | "thumbnailurl"
                );
                collect_structured_images(value, context, urls);
            }
        }
        _ => {}
    }
}

fn unique_image_families<'a>(urls: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut families = Vec::new();
    for url in urls {
        let Ok(parsed) = Url::parse(url) else { continue };
        let family = image_family(&parsed);
        if !families.contains(&family) {
            families.push(family);
        }
    }
    families
}

fn image_family(url: &Url) -> String {
    let mut path = url.path().to_ascii_lowercase();
    // WordPress and similar CDNs commonly add a thumbnail suffix while
    // retaining the original stem. Strip only a numeric WxH suffix for
    // ambiguity checks; no download URL is synthesized from this value.
    if let Some(dot) = path.rfind('.') {
        let (stem, extension) = path.split_at(dot);
        if let Some(hyphen) = stem.rfind('-') {
            let suffix = &stem[hyphen + 1..];
            if dimensions_from_text(suffix) != (0, 0) {
                path = format!("{stem_prefix}{extension}", stem_prefix = &stem[..hyphen]);
            }
        }
    }
    format!("{}:{path}", url.host_str().unwrap_or_default())
}

fn source_url_from(base: &Url, raw: &str) -> Option<Url> {
    let decoded = html_unescape(raw.trim());
    if decoded.is_empty() {
        return None;
    }
    let url = base.join(&decoded).ok()?;
    valid_source_url_parsed(&url).then_some(url)
}

fn html_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&#x26;", "&")
        .replace("&#38;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn html_tags<'a>(html: &'a str, wanted: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let marker = format!("<{wanted}");
    let mut tags = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find(&marker) {
        let start = cursor + offset;
        let after = start + marker.len();
        if lower
            .as_bytes()
            .get(after)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>' && *byte != b'/')
        {
            cursor = after;
            continue;
        }
        let Some(end_offset) = lower[start..].find('>') else {
            break;
        };
        let end = start + end_offset + 1;
        tags.push(&html[start..end]);
        cursor = end;
    }
    tags
}

fn html_meta_dimension(html: &str, property: &str) -> u32 {
    html_tags(html, "meta")
        .into_iter()
        .find_map(|tag| {
            let key = html_attribute(tag, "property")
                .or_else(|| html_attribute(tag, "name"))?;
            key.eq_ignore_ascii_case(property)
                .then(|| html_attribute(tag, "content"))
                .flatten()
        })
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0 && *value <= 16384)
        .unwrap_or(0)
}

fn dimensions_from_text(text: &str) -> (u32, u32) {
    let bytes = text.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] != b'x' && bytes[index] != b'X' {
            continue;
        }
        let mut left = index;
        while left > 0 && bytes[left - 1].is_ascii_digit() {
            left -= 1;
        }
        let mut right = index + 1;
        while right < bytes.len() && bytes[right].is_ascii_digit() {
            right += 1;
        }
        if left == index || right == index + 1 {
            continue;
        }
        let Ok(width) = text[left..index].parse::<u32>() else {
            continue;
        };
        let Ok(height) = text[index + 1..right].parse::<u32>() else {
            continue;
        };
        if width > 0 && height > 0 && width <= 16384 && height <= 16384 {
            return (width, height);
        }
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_relay_page_recovers_original_and_matching_source() {
        let html = r#"<meta property="og:image" content="https://i.pinimg.com/736x/43/be/a3/asset.jpg">
        <script data-relay-completed-request="true">window.__PWS_RELAY_REGISTER_COMPLETED_REQUEST__("key",{"data":{"pin":{"entityId":"123","link":"https://wallhaven.cc/w/zpvp3g","images_orig":{"url":"https://i.pinimg.com/originals/43/be/a3/asset.png"}}}});</script>"#;
        let result = observed_pin_image(html, "123").unwrap();
        assert!(result.primary_is_original);
        assert_eq!(result.primary, "https://i.pinimg.com/originals/43/be/a3/asset.png");
        assert_eq!(result.source_url.as_deref(), Some("https://wallhaven.cc/w/zpvp3g"));
        assert_eq!(observed_pin_source(html, "999"), None);
    }

    #[test]
    fn asset_identity_matches_size_paths_and_extensions() {
        assert_eq!(
            asset_identity("https://i.pinimg.com/736x/ab/cd/image.jpg"),
            asset_identity("https://i.pinimg.com/originals/ab/cd/image.png")
        );
        assert_ne!(
            asset_identity("https://i.pinimg.com/originals/ab/cd/image.jpg"),
            asset_identity("https://i.pinimg.com/originals/ab/cd/other.jpg")
        );
    }
    #[test]
    fn image_hosts_are_restricted() {
        assert!(valid_image_url("https://i.pinimg.com/a.jpg"));
        assert!(!valid_image_url("https://pinimg.com.evil.org/a"));
        assert!(!valid_image_url("http://i.pinimg.com/a"));
        assert!(!valid_image_url("https://localhost/a"));
    }

    #[test]
    fn download_url_does_not_guess_an_original_variant() {
        let thumbnail = Pin {
            url: "https://i.pinimg.com/736x/hash.jpg".into(),
            ..Default::default()
        };
        assert_eq!(
            preferred_download_url(&thumbnail),
            "https://i.pinimg.com/736x/hash.jpg"
        );

        let exact = Pin {
            url: "https://i.pinimg.com/originals/hash.png".into(),
            original_url_exact: true,
            ..Default::default()
        };
        assert_eq!(
            preferred_download_url(&exact),
            "https://i.pinimg.com/originals/hash.png"
        );

        let legacy = Pin {
            url: "https://i.pinimg.com/originals/hash.jpg".into(),
            fallback_url: Some("https://i.pinimg.com/736x/hash.jpg".into()),
            ..Default::default()
        };
        assert_eq!(
            preferred_download_url(&legacy),
            "https://i.pinimg.com/736x/hash.jpg"
        );

        let unrelated_fallback = Pin {
            url: "https://i.pinimg.com/originals/hash.jpg".into(),
            fallback_url: Some("https://i.pinimg.com/736x/other.jpg".into()),
            ..Default::default()
        };
        assert_eq!(
            preferred_download_url(&unrelated_fallback),
            "https://i.pinimg.com/originals/hash.jpg"
        );
    }

    #[test]
    fn download_url_uses_only_matching_original_metadata() {
        let pin = Pin {
            url: "https://i.pinimg.com/736x/hash.jpg".into(),
            max_width: 2400,
            max_height: 1600,
            max_dimensions_source: crate::model::DimensionSource::PinterestOriginal,
            max_dimensions_url: Some("https://i.pinimg.com/originals/hash.png".into()),
            ..Default::default()
        };
        assert_eq!(
            preferred_download_url(&pin),
            "https://i.pinimg.com/originals/hash.png"
        );
    }

    #[test]
    fn page_resolution_keeps_only_observed_matching_variants() {
        let html = r#"
          <meta content="https://i.pinimg.com/736x/12/fa/5e/12fa5e.jpg"
                property="og:image">
          <meta content="736" property="og:image:width">
          <meta content="1307" property="og:image:height">
          <script type="application/json">
            {"entityId":"123","images_orig":{"url":"https://i.pinimg.com/originals/12/fa/5e/12fa5e.png","width":2000,"height":3550}}
          </script>
          <script>{"images_orig":{"url":"https://i.pinimg.com/originals/aa/bb/cc/other.png"}}</script>
        "#;
        assert_eq!(
            observed_pin_image(html, "123"),
            Some(ObservedPinImage {
                source_url: None,
                primary: "https://i.pinimg.com/originals/12/fa/5e/12fa5e.png".into(),
                fallback: Some("https://i.pinimg.com/736x/12/fa/5e/12fa5e.jpg".into()),
                primary_is_original: true,
                thumbnail_width: 736,
                thumbnail_height: 1307,
                max_width: 2000,
                max_height: 3550,
                max_dimensions_url: Some(
                    "https://i.pinimg.com/originals/12/fa/5e/12fa5e.png".into(),
                ),
            })
        );
    }

    #[test]
    fn page_resolution_requires_an_observed_image() {
        assert!(
            observed_pin_image("<meta property=\"og:title\" content=\"Pin\">", "123").is_none()
        );
    }

    #[test]
    fn page_resolution_does_not_copy_nearby_thumbnail_dimensions_to_original() {
        let html = r#"
          <meta content="https://i.pinimg.com/736x/12/fa/5e/12fa5e.jpg"
                property="og:image">
          <meta content="736" property="og:image:width">
          <meta content="1307" property="og:image:height">
          <script type="application/json">
            {"entityId":"123","images_orig":{"url":"https://i.pinimg.com/originals/12/fa/5e/12fa5e.png"},"images":{"small":{"width":736,"height":1307}}}
          </script>
        "#;
        let observed = observed_pin_image(html, "123").unwrap();
        assert_eq!((observed.max_width, observed.max_height), (0, 0));
        assert_eq!(
            (observed.thumbnail_width, observed.thumbnail_height),
            (736, 1307)
        );
    }

    #[test]
    fn source_url_validation_rejects_local_targets_and_pinterest() {
        assert!(valid_source_url("https://wallhaven.cc/w/zpvp3g"));
        assert!(valid_source_url("https://images.example.org/wallpaper.jpg"));
        assert!(!valid_source_url("http://wallhaven.cc/w/zpvp3g"));
        assert!(!valid_source_url("https://localhost/wallpaper.jpg"));
        assert!(!valid_source_url("https://127.0.0.1/wallpaper.jpg"));
        assert!(!valid_source_url("https://[::ffff:127.0.0.1]/wallpaper.jpg"));
        assert!(!valid_source_url("https://www.pinterest.com/pin/123/"));
        assert!(!valid_source_url("https://user:password@wallhaven.cc/w/zpvp3g"));
    }

    #[test]
    fn external_source_backoff_is_scoped_to_the_pinned_asset() {
        let source = format!(
            "https://www.pixelstalk.net/gallery/backoff-{}",
            rand::random::<u64>()
        );
        let first = Pin {
            url: "https://i.pinimg.com/736x/first.jpg".into(),
            source_url: Some(source.clone()),
            ..Default::default()
        };
        let second = Pin {
            url: "https://i.pinimg.com/736x/second.jpg".into(),
            source_url: Some(source),
            ..Default::default()
        };
        mark_external_source_unavailable(&first);
        assert!(external_source_backoff(&first));
        assert!(!external_source_backoff(&second));
    }

    #[test]
    fn wallpaperscraft_uses_explicit_image_link_and_published_resolution() {
        let page = Url::parse(
            "https://wallpaperscraft.com/download/night_city_window_rain_131009/1366x768",
        )
        .unwrap();
        let html = r#"
          <a href="https://images.wallpaperscraft.com/image/single/night_city_window_rain_131009_1366x768.jpg">Download</a>
          <a href="/download/night_city_window_rain_131009/3840x2160">3840x2160</a>
          <a href="/download/night_city_window_rain_131009/3840x2400">3840x2400</a>
          <a href="/download/unrelated_99/7680x4320">related</a>
        "#;
        let resolved = resolve_source_document(&page, html.as_bytes()).unwrap();
        assert_eq!(
            resolved.image_url,
            "https://images.wallpaperscraft.com/image/single/night_city_window_rain_131009_1366x768.jpg"
        );
        assert_eq!((resolved.width, resolved.height), (1366, 768));
        let pages = wallpaperscraft_resolution_pages(&page, html);
        assert_eq!(pages.len(), 2);
        assert_eq!(dimensions_from_text(pages[0].path()), (3840, 2400));
    }

    #[test]
    fn wallpaperscraft_landing_page_keeps_observed_download_links() {
        let page = Url::parse("https://wallpaperscraft.com/wallpaper/night_city_window_rain_131009")
            .unwrap();
        let html = r#"
          <a href="/download/night_city_window_rain_131009/3840x2160">3840x2160</a>
          <a href="/download/other_wallpaper/7680x4320">related</a>
        "#;
        let pages = wallpaperscraft_resolution_pages(&page, html);
        assert_eq!(pages.len(), 1);
        assert_eq!(
            pages[0].as_str(),
            "https://wallpaperscraft.com/download/night_city_window_rain_131009/3840x2160"
        );
    }

    #[test]
    fn wallhaven_prefers_the_exact_full_image_over_social_thumbnail() {
        let page = Url::parse("https://wallhaven.cc/w/zpvp3g").unwrap();
        let html = r#"
          <meta property="og:image" content="https://th.wallhaven.cc/lg/zp/zpvp3g.jpg">
          <img id="wallpaper" src="https://w.wallhaven.cc/full/zp/wallhaven-zpvp3g.png">
        "#;
        let resolved = resolve_source_document(&page, html.as_bytes()).unwrap();
        assert_eq!(
            resolved.image_url,
            "https://w.wallhaven.cc/full/zp/wallhaven-zpvp3g.png"
        );
        assert_eq!((resolved.width, resolved.height), (0, 0));
    }

    #[test]
    fn pixelstalk_gallery_is_rejected_as_ambiguous() {
        let page = Url::parse("https://www.pixelstalk.net/wallpapers/example/").unwrap();
        let html = r#"
          <meta property="og:image" content="https://www.pixelstalk.net/wp-content/uploads/example.jpg">
          <a href="https://www.pixelstalk.net/wp-content/uploads/example.jpg">one</a>
          <a href="https://www.pixelstalk.net/wp-content/uploads/example-2.jpg">two</a>
        "#;
        assert!(resolve_source_document(&page, html.as_bytes()).is_none());
    }

    #[test]
    fn generic_srcset_chooses_the_largest_same_image_family() {
        let page = Url::parse("https://example.org/wallpaper/").unwrap();
        let html = r#"
          <meta property="og:image" content="/assets/forest-640x360.jpg">
          <img srcset="/assets/forest-640x360.jpg 640w, /assets/forest.jpg 1920w">
        "#;
        let resolved = resolve_source_document(&page, html.as_bytes()).unwrap();
        assert_eq!(resolved.image_url, "https://example.org/assets/forest.jpg");
    }

    #[test]
    fn pixelstalk_gallery_matching_can_select_a_nonfirst_tile() {
        let page = Url::parse("https://www.pixelstalk.net/wallpapers/example/").unwrap();
        let html = r#"
          <a href="/wp-content/uploads/first.jpg">
            <img src="/wp-content/uploads/first-320x180.jpg">
          </a>
          <a href="/wp-content/uploads/target.jpg">
            <img src="/wp-content/uploads/target-320x180.jpg">
          </a>
          <img src="/wp-content/uploads/target-320x180.jpg">
        "#;
        let candidates = gallery_candidates(&page, html);
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[1].preview_urls,
            vec!["https://www.pixelstalk.net/wp-content/uploads/target-320x180.jpg"]
        );
        let target = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 36, |x, y| {
            image::Rgb([
                ((x * 4 + y * 2) % 256) as u8,
                ((y * 7) % 256) as u8,
                (((x + y) * 3) % 256) as u8,
            ])
        }));
        let unrelated = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 36, |x, y| {
            image::Rgb([
                ((x * 4 + y * 2 + 120) % 256) as u8,
                ((y * 7 + 120) % 256) as u8,
                (((x + y) * 3 + 120) % 256) as u8,
            ])
        }));
        let target_fingerprint = image_fingerprint(&target).unwrap();
        let unrelated_fingerprint = image_fingerprint(&unrelated).unwrap();
        let matches = vec![
            (
                fingerprint_distance(&target_fingerprint, &unrelated_fingerprint).unwrap(),
                &candidates[0],
            ),
            (0.0, &candidates[1]),
        ];
        let match_candidate = choose_gallery_match(matches).unwrap();
        assert_eq!(
            match_candidate.image_url,
            "https://www.pixelstalk.net/wp-content/uploads/target.jpg"
        );
    }

    #[test]
    fn gallery_matching_rejects_two_similar_tiles_as_ambiguous() {
        let page = Url::parse("https://www.pixelstalk.net/wallpapers/example/").unwrap();
        let html = r#"
          <a href="/wp-content/uploads/one-full.jpg"><img src="/wp-content/uploads/one.jpg"></a>
          <a href="/wp-content/uploads/two-full.jpg"><img src="/wp-content/uploads/two.jpg"></a>
        "#;
        let candidates = gallery_candidates(&page, html);
        let base = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 36, |x, y| {
            image::Rgb([
                ((x * 4 + y * 2) % 256) as u8,
                ((y * 7) % 256) as u8,
                (((x + y) * 3) % 256) as u8,
            ])
        }));
        let nearly_same = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 36, |x, y| {
            image::Rgb([
                ((x * 4 + y * 2 + 2) % 256) as u8,
                ((y * 7 + 2) % 256) as u8,
                (((x + y) * 3 + 2) % 256) as u8,
            ])
        }));
        let base_fingerprint = image_fingerprint(&base).unwrap();
        let similar_fingerprint = image_fingerprint(&nearly_same).unwrap();
        let similar_score =
            fingerprint_distance(&base_fingerprint, &similar_fingerprint).unwrap();
        assert!(similar_score <= SOURCE_MATCH_THRESHOLD);
        assert!(choose_gallery_match(vec![
            (0.0, &candidates[0]),
            (similar_score, &candidates[1]),
        ])
        .is_none());

        let solid = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            64,
            36,
            image::Rgb([255, 0, 0]),
        ));
        let solid_fingerprint = image_fingerprint(&solid).unwrap();
        assert!(solid_fingerprint.variance < 0.002);
        assert!(fingerprint_distance(&base_fingerprint, &solid_fingerprint).is_none());
    }
}
