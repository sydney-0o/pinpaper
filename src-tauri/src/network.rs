use reqwest::blocking::Client;
use std::time::Duration;
use url::Url;
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
}
