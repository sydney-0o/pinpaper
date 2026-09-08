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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_hosts_are_restricted() {
        assert!(valid_image_url("https://i.pinimg.com/a.jpg"));
        assert!(!valid_image_url("https://pinimg.com.evil.org/a"));
        assert!(!valid_image_url("http://i.pinimg.com/a"));
        assert!(!valid_image_url("https://localhost/a"));
    }
}
