use reqwest::blocking::Client;
use std::{io::Read, time::Duration};
use url::Url;
const MAX_PIN_PAGE: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedPinImage {
    pub primary: String,
    pub fallback: Option<String>,
    pub width: u32,
    pub height: u32,
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
// Upgrade only Pinterest's known size-path convention, preserving the asset identity.
pub fn original_url(source: &str) -> String {
    if !valid_image_url(source) {
        return source.to_owned();
    }
    let Ok(mut url) = Url::parse(source) else {
        return source.to_owned();
    };
    let path = url.path().to_owned();
    let Some((size, rest)) = path.trim_start_matches('/').split_once('/') else {
        return source.to_owned();
    };
    if let Some((width, _)) = size.split_once('x') {
        if !width.is_empty() && width.bytes().all(|c| c.is_ascii_digit()) {
            url.set_path(&format!("/originals/{rest}"));
        }
    }
    url.to_string()
}

pub fn is_original_url(source: &str) -> bool {
    let Ok(url) = Url::parse(source) else {
        return false;
    };
    valid_image_url(source)
        && url.path_segments().and_then(|mut segments| segments.next()) == Some("originals")
}

/// Read one public pin page and return only image URLs present in that page.
/// The request has no Pinterest cookies and is deliberately limited to one page
/// per foreground wallpaper change. Private pins can still be repaired by a
/// subsequent import from the authenticated Pinterest window.
pub fn resolve_pin_image(pin_id: &str) -> Result<ObservedPinImage, String> {
    if pin_id.is_empty() || pin_id.len() > 32 || !pin_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid Pinterest pin ID".into());
    }
    let page = format!("https://www.pinterest.com/pin/{pin_id}/");
    let response = client()?
        .get(page)
        .send()
        .map_err(|_| "Pinterest pin page could not be reached".to_owned())?;
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
    let asset = asset_stem(&fallback)?;
    let original = observed_originals(html)
        .into_iter()
        .find(|url| asset_stem(url).as_deref() == Some(asset.as_str()));
    Some(ObservedPinImage {
        primary: original.clone().unwrap_or_else(|| fallback.clone()),
        fallback: original.and_then(|url| (url != fallback).then_some(fallback)),
        width,
        height,
    })
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

fn observed_originals(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find("\"images_orig\"") {
        let start = cursor + offset;
        let end = (start + 4096).min(html.len());
        if let Some(url) = pinimg_url(&html[start..end]) {
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
        cursor = start + "\"images_orig\"".len();
    }
    urls
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

fn asset_stem(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let name = parsed.path_segments()?.next_back()?;
    Some(
        name.rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(name)
            .to_owned(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upgrade_thumbnail_without_changing_asset_or_external_hosts() {
        assert_eq!(
            original_url("https://i.pinimg.com/736x/ab/cd/image.jpg"),
            "https://i.pinimg.com/originals/ab/cd/image.jpg"
        );
        assert_eq!(
            original_url("https://i.pinimg.com/236x350/a.png"),
            "https://i.pinimg.com/originals/a.png"
        );
        assert_eq!(
            original_url("https://i.pinimg.com/originals/a.jpg"),
            "https://i.pinimg.com/originals/a.jpg"
        );
        assert_eq!(
            original_url("https://example.com/736x/a.jpg"),
            "https://example.com/736x/a.jpg"
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
    fn page_resolution_keeps_only_observed_matching_variants() {
        let html = r#"
          <meta content="https://i.pinimg.com/736x/12/fa/5e/12fa5e.jpg"
                property="og:image">
          <meta content="736" property="og:image:width">
          <meta content="1307" property="og:image:height">
          <script type="application/json">
            {"entityId":"123","images_orig":{"url":"https://i.pinimg.com/originals/12/fa/5e/12fa5e.png"}}
          </script>
          <script>{"images_orig":{"url":"https://i.pinimg.com/originals/aa/bb/cc/other.png"}}</script>
        "#;
        assert_eq!(
            observed_pin_image(html, "123"),
            Some(ObservedPinImage {
                primary: "https://i.pinimg.com/originals/12/fa/5e/12fa5e.png".into(),
                fallback: Some("https://i.pinimg.com/736x/12/fa/5e/12fa5e.jpg".into()),
                width: 736,
                height: 1307,
            })
        );
    }

    #[test]
    fn page_resolution_requires_an_observed_image() {
        assert!(
            observed_pin_image("<meta property=\"og:title\" content=\"Pin\">", "123").is_none()
        );
    }
}
