//! User-driven Pinterest browser session. No password/DOM-input capture.
use crate::{model::Pin, network};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{
    webview::{NewWindowFeatures, NewWindowResponse},
    Manager, WebviewUrl, WebviewWindowBuilder,
};
use url::Url;

pub const LABEL: &str = "pinterest-session";
pub const SOURCE: &str = "browser-session";
const HOME: &str = "https://www.pinterest.com/";
const COOKIE_NAMES: &[&str] = &["_pinterest_sess", "_auth", "csrftoken"];

pub struct SessionCookie {
    name: String,
    value: String,
    domain: String,
    expires_at: Option<i64>,
}
fn valid_cookie(c: &SessionCookie) -> bool {
    COOKIE_NAMES.contains(&c.name.as_str())
        && pinterest_host(c.domain.trim_start_matches('.'))
        && !c.value.is_empty()
        && c.value.len() <= 8192
        && !c.value.chars().any(|ch| ch.is_control() || ch == ';')
        && c.expires_at
            .map(|n| n > chrono::Utc::now().timestamp())
            .unwrap_or(true)
}
fn signed_in(c: &[SessionCookie]) -> bool {
    c.iter()
        .any(|c| c.name == "_pinterest_sess" && valid_cookie(c))
        && c.iter()
            .any(|c| c.name == "_auth" && c.value == "1" && valid_cookie(c))
}
pub fn pinterest_host(host: &str) -> bool {
    host == "pinterest.com" || host.ends_with(".pinterest.com")
}
pub fn trusted_page(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some_and(pinterest_host)
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}
// Pinterest locale subdomains use the same import policy as www.
// Verification providers may navigate but receive no native permissions.
fn allowed_navigation(url: &Url) -> bool {
    url.as_str() == "about:blank"
        || (url.scheme() == "https"
            && url.host_str().is_some_and(|host| {
                [
                    "pinterest.com",
                    "google.com",
                    "recaptcha.net",
                    "hcaptcha.com",
                    "arkoselabs.com",
                    "funcaptcha.com",
                    "challenges.cloudflare.com",
                ]
                .iter()
                .any(|base| host == *base || host.ends_with(&format!(".{base}")))
            })
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none())
}
fn explain_blocked_login(app: &tauri::AppHandle) {
    // Never include a redirect URL: it may contain authorization credentials.
    if let Some(engine) = app.try_state::<std::sync::Arc<crate::Engine>>() {
        if let Ok(mut error) = engine.error.lock() {
            *error = Some("Pinpaper could not open a sign-in verification page. Close the Pinterest window and retry. If CAPTCHA still fails to load, this embedded login may be incompatible with the verification flow.".into());
        }
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.set_focus();
    }
}
const POPUP_PREFIX: &str = "pinterest-verification-";
fn open_popup(
    app: &tauri::AppHandle,
    url: Url,
    features: NewWindowFeatures,
) -> NewWindowResponse<tauri::Wry> {
    if !allowed_navigation(&url)
        || app
            .webview_windows()
            .keys()
            .filter(|label| label.starts_with(POPUP_PREFIX))
            .count()
            >= 4
    {
        explain_blocked_login(app);
        return NewWindowResponse::Deny;
    }
    let navigation_app = app.clone();
    let popup_app = app.clone();
    // Preserve the opener's native configuration, private store and window.opener.
    // A fresh independent webview would break popup verification callbacks.
    let window = WebviewWindowBuilder::new(
        app,
        format!("{POPUP_PREFIX}{}", rand::random::<u64>()),
        WebviewUrl::External(Url::parse("about:blank").unwrap()),
    )
    .title(crate::language::text(
        crate::language::system_language(),
        "verifyTitle",
    ))
    .inner_size(700.0, 740.0)
    .incognito(true)
    .window_features(features)
    .on_navigation(move |url| {
        let allowed = allowed_navigation(url);
        if !allowed {
            explain_blocked_login(&navigation_app);
        }
        allowed
    })
    .on_new_window(move |url, features| open_popup(&popup_app, url, features))
    .build();
    match window {
        Ok(window) => NewWindowResponse::Create { window },
        Err(_) => {
            explain_blocked_login(app);
            NewWindowResponse::Deny
        }
    }
}
pub fn close_popups(app: &tauri::AppHandle) {
    for (label, window) in app.webview_windows() {
        if label.starts_with(POPUP_PREFIX) {
            let _ = window.destroy();
        }
    }
}
pub fn open_browser(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(LABEL) {
        w.show().map_err(|_| "Cannot show Pinterest")?;
        return w.set_focus().map_err(|_| "Cannot focus Pinterest".into());
    }
    let navigation_app = app.clone();
    let popup_app = app.clone();
    // An ephemeral webview: existing Safari/Chrome cookies are never accessed.
    let w = WebviewWindowBuilder::new(
        app,
        LABEL,
        WebviewUrl::External(Url::parse("about:blank").unwrap()),
    )
    .title(crate::language::text(
        crate::language::system_language(),
        "browserTitle",
    ))
    .inner_size(1050.0, 760.0)
    .incognito(true)
    .on_navigation(move |url| {
        let allowed = allowed_navigation(url);
        if !allowed {
            explain_blocked_login(&navigation_app);
        }
        allowed
    })
    .on_new_window(move |url, features| open_popup(&popup_app, url, features))
    .build()
    .map_err(|_| "Cannot open the private Pinterest window")?;
    let result = w
        .navigate(Url::parse(HOME).unwrap())
        .map_err(|_| "Cannot open Pinterest".to_owned());
    if result.is_err() {
        let _ = w.destroy();
    }
    result
}
pub fn session_window(label: &str) -> bool {
    label == LABEL || label.starts_with(POPUP_PREFIX)
}
fn page_priority(url: &Url) -> u8 {
    if !trusted_page(url) {
        return 0;
    }
    if url.path().starts_with("/login") || url.path().starts_with("/signup") {
        1
    } else {
        2
    }
}
fn content_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    let mut candidates: Vec<_> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| session_window(label))
        .filter_map(|(label, window)| {
            let url = window.url().ok()?;
            let priority = page_priority(&url);
            (priority > 0).then_some((
                (priority, window.is_focused().unwrap_or(false), label),
                window,
            ))
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.pop().map(|(_, window)| window).ok_or_else(|| {
        let hosts: Vec<_> = app.webview_windows().into_iter().filter(|(label, _)| session_window(label))
            .filter_map(|(_, w)| w.url().ok().map(|u| u.host_str().unwrap_or("blank").to_owned())).collect();
        format!("No Pinterest page found in the sign-in windows. Return to Pinterest Home after verification. Open sites: {}", hosts.join(", "))
    })
}
pub fn capture(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    let w = content_window(app)?;
    // Authentication is established by scoped cookies, not a possibly stale
    // native URL after Pinterest's client-side login transition.
    // Wry 0.55 macOS cookies_for_url compares domain strings exactly, dropping
    // parent-domain cookies such as .pinterest.com for www.pinterest.com.
    // Read only this private webview's store, then enforce our scope locally.
    let cookies: Vec<_> = w
        .cookies()
        .map_err(|_| "Cannot read this Pinterest window's session")?
        .into_iter()
        .filter(|c| COOKIE_NAMES.contains(&c.name()))
        .map(|c| SessionCookie {
            name: c.name().into(),
            value: c.value().into(),
            domain: c.domain().unwrap_or("www.pinterest.com").into(),
            expires_at: c.expires_datetime().map(|d| d.unix_timestamp()),
        })
        .filter(valid_cookie)
        .collect();
    if !signed_in(&cookies) {
        return Err("Pinterest has not confirmed sign-in. Finish login in the Pinterest window and try again.".into());
    }
    Ok(w)
}

#[derive(Deserialize)]
pub struct PagePin {
    id: String,
    title: String,
    description: String,
    url: String,
    #[serde(default)]
    fallback_url: Option<String>,
    width: u32,
    height: u32,
}
#[derive(Deserialize)]
pub struct PageReport {
    pub nonce: String,
    pub page_url: String,
    pub pins: Vec<PagePin>,
    pub error: Option<String>,
}
struct Pending {
    window_label: String,
    nonce: String,
    started: Instant,
    sender: std::sync::mpsc::Sender<Result<Vec<Pin>, String>>,
}
#[derive(Default)]
pub struct ImportBridge {
    pending: Mutex<Option<Pending>>,
}
impl ImportBridge {
    pub fn receive(&self, window_label: &str, report: PageReport) -> Result<(), String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "Import worker unavailable")?;
        let request = pending.as_ref().ok_or("No import requested")?;
        if window_label != request.window_label
            || report.nonce != request.nonce
            || request.started.elapsed() > Duration::from_secs(15)
        {
            return Err("Invalid or expired import request".into());
        }
        let result = normalize_report(report);
        let request = pending.take().unwrap();
        request
            .sender
            .send(result)
            .map_err(|_| "Import request expired".into())
    }
    pub fn import(&self, w: &tauri::WebviewWindow) -> Result<Vec<Pin>, String> {
        let nonce: String = {
            use rand::Rng;
            rand::thread_rng()
                .sample_iter(&rand::distributions::Alphanumeric)
                .take(48)
                .map(char::from)
                .collect()
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        *self
            .pending
            .lock()
            .map_err(|_| "Import worker unavailable")? = Some(Pending {
            window_label: w.label().into(),
            nonce: nonce.clone(),
            started: Instant::now(),
            sender,
        });
        let script = include_str!("capture_page.js").replace(
            "__PINPAPER_NONCE__",
            &serde_json::to_string(&nonce).unwrap(),
        );
        let result = match w.eval(script) {
            Ok(()) => receiver.recv_timeout(Duration::from_secs(15)).map_err(|_| "Pinterest did not answer the import request. Wait for the page to load and try again.".to_owned()).and_then(|r|r),
            Err(_) => Err("Cannot inspect the Pinterest page".to_owned()),
        };
        self.pending
            .lock()
            .map_err(|_| "Import worker unavailable")?
            .take();
        result
    }
}
fn normalize_report(report: PageReport) -> Result<Vec<Pin>, String> {
    let url = Url::parse(&report.page_url).map_err(|_| "Invalid Pinterest location")?;
    if !trusted_page(&url) || report.error.is_some() {
        return Err("Open your Home feed or a board after signing in".into());
    }
    if report.pins.len() > 200 {
        return Err("Too many pins in one import".into());
    }
    let mut seen = std::collections::HashSet::new();
    let pins: Vec<_> = report
        .pins
        .into_iter()
        .filter(|p| {
            !p.id.is_empty()
                && p.id.len() <= 32
                && p.id.bytes().all(|c| c.is_ascii_digit())
                && p.url.len() < 4096
                && network::valid_image_url(&p.url)
                && p.width <= 16384
                && p.height <= 16384
                && seen.insert(p.id.clone())
        })
        .map(|p| {
            let fallback_url = p
                .fallback_url
                .as_deref()
                .filter(|url| *url != p.url.as_str() && network::valid_image_url(url))
                .map(str::to_owned);
            Pin {
                dimensions_verified: false,
                id: p.id,
                board_id: SOURCE.into(),
                title: if p.title.trim().is_empty() {
                    "Untitled pin".into()
                } else {
                    p.title.chars().take(300).collect()
                },
                description: p.description.chars().take(2000).collect(),
                url: p.url,
                fallback_url,
                width: if p.width > 0 && p.height > 0 {
                    p.width
                } else {
                    0
                },
                height: if p.width > 0 && p.height > 0 {
                    p.height
                } else {
                    0
                },
            }
        })
        .collect();
    if pins.is_empty() {
        return Err(
            "No image pins found. Scroll the Pinterest page to load pins, then import again."
                .into(),
        );
    }
    Ok(pins)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn cookie(name: &str, value: &str) -> SessionCookie {
        SessionCookie {
            name: name.into(),
            value: value.into(),
            domain: ".pinterest.com".into(),
            expires_at: None,
        }
    }
    #[test]
    fn session_requires_auth_and_session_cookie() {
        assert!(!signed_in(&[cookie("_auth", "1")]));
        assert!(!signed_in(&[
            cookie("_auth", "0"),
            cookie("_pinterest_sess", "test")
        ]));
        assert!(signed_in(&[
            cookie("_auth", "1"),
            cookie("_pinterest_sess", "test")
        ]));
    }
    #[test]
    fn cookies_are_scoped_and_expire() {
        let mut c = cookie("_auth", "1");
        c.domain = "pinterest.com.evil.test".into();
        assert!(!valid_cookie(&c));
        c.domain = ".pinterest.com".into();
        assert!(valid_cookie(&c));
        c.domain = "www.pinterest.com".into();
        assert!(valid_cookie(&c));
        c.domain = ".google.com".into();
        assert!(!valid_cookie(&c));
        c.domain = ".pinterest.com".into();
        c.expires_at = Some(1);
        assert!(!valid_cookie(&c));
    }
    #[test]
    fn navigation_is_restricted() {
        for s in [
            "http://www.pinterest.com/",
            "https://evil.test/",
            "https://www.pinterest.com.evil.test/",
            "file:///tmp/a",
            "https://www.pinterest.com:444/",
        ] {
            assert!(!trusted_page(&Url::parse(s).unwrap()));
            assert!(!allowed_navigation(&Url::parse(s).unwrap()));
        }
        assert!(trusted_page(&Url::parse(HOME).unwrap()));
        for host in ["ru.pinterest.com", "es.pinterest.com", "in.pinterest.com"] {
            assert!(trusted_page(
                &Url::parse(&format!("https://{host}/")).unwrap()
            ));
            let mut c = cookie("_auth", "1");
            c.domain = format!(".{host}");
            assert!(valid_cookie(&c));
        }
        assert!(!trusted_page(
            &Url::parse("https://ru.pinterest.com.evil.test/").unwrap()
        ));
        let bare = Url::parse("https://pinterest.com/login/").unwrap();
        assert!(allowed_navigation(&bare));
        assert!(trusted_page(&bare));
        assert_eq!(page_priority(&bare), 1);
        assert_eq!(page_priority(&Url::parse(HOME).unwrap()), 2);
        for address in [
            "https://www.google.com/recaptcha/",
            "https://newassets.hcaptcha.com/",
            "https://challenges.cloudflare.com/",
        ] {
            let url = Url::parse(address).unwrap();
            assert!(allowed_navigation(&url));
            assert!(!trusted_page(&url));
        }
        assert!(!allowed_navigation(
            &Url::parse("https://google.com.evil.test/").unwrap()
        ));
        assert!(!allowed_navigation(
            &Url::parse("https://user@pinterest.com/").unwrap()
        ));
    }
    fn report() -> PageReport {
        serde_json::from_value(serde_json::json!({"nonce":"n","page_url":HOME,"error":null,"pins":[{"id":"123","title":"Forest","description":"","url":"https://i.pinimg.com/originals/a.jpg","width":0,"height":0}]})).unwrap()
    }
    #[test]
    fn reports_are_untrusted() {
        let mut r = report();
        r.pins[0].url = "https://localhost/secret".into();
        assert!(normalize_report(r).is_err());
        let mut r = report();
        r.page_url = "https://evil.test/".into();
        assert!(normalize_report(r).is_err());
        assert_eq!(normalize_report(report()).unwrap()[0].board_id, SOURCE);
    }
    #[test]
    fn bridge_rejects_unsolicited_and_wrong_nonce() {
        let b = ImportBridge::default();
        assert!(b.receive(LABEL, report()).is_err());
        let (tx, _rx) = std::sync::mpsc::channel();
        *b.pending.lock().unwrap() = Some(Pending {
            window_label: LABEL.into(),
            nonce: "different".into(),
            started: Instant::now(),
            sender: tx,
        });
        assert!(b.receive(LABEL, report()).is_err());
    }
}

#[cfg(test)]
mod bridge_tests {
    use super::*;
    #[test]
    fn report_nonce_is_consumed_once() {
        let b = ImportBridge::default();
        let (tx, rx) = std::sync::mpsc::channel();
        *b.pending.lock().unwrap() = Some(Pending {
            window_label: LABEL.into(),
            nonce: "one-use".into(),
            started: Instant::now(),
            sender: tx,
        });
        let report = || PageReport {
            nonce: "one-use".into(),
            page_url: HOME.into(),
            error: None,
            pins: vec![PagePin {
                id: "123".into(),
                title: "".into(),
                description: "".into(),
                url: "https://i.pinimg.com/a.jpg".into(),
                fallback_url: None,
                width: 0,
                height: 0,
            }],
        };
        assert!(b.receive("pinterest-verification-other", report()).is_err());
        b.receive(LABEL, report()).unwrap();
        assert_eq!(rx.recv().unwrap().unwrap().len(), 1);
        assert!(b.receive(LABEL, report()).is_err());
    }
}
