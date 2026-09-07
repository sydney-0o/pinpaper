use std::sync::OnceLock;
fn resolve(locale: &str) -> &'static str {
    match locale
        .split(['-', '_', '.', ':'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "ru" => "ru",
        "es" => "es",
        "zh" => "zh",
        "hi" => "hi",
        _ => "en",
    }
}
pub fn system_language() -> &'static str {
    static LANGUAGE: OnceLock<&'static str> = OnceLock::new();
    LANGUAGE.get_or_init(|| resolve(&system_locale()))
}
#[cfg(target_os = "macos")]
fn system_locale() -> String {
    objc2_foundation::NSLocale::preferredLanguages()
        .iter()
        .next()
        .map(|l| l.to_string())
        .unwrap_or_default()
}
#[cfg(target_os = "windows")]
fn system_locale() -> String {
    let mut buffer = [0u16; 85];
    let len = unsafe {
        windows_sys::Win32::Globalization::GetUserDefaultLocaleName(
            buffer.as_mut_ptr(),
            buffer.len() as i32,
        )
    };
    if len > 0 {
        String::from_utf16_lossy(&buffer[..len as usize - 1])
    } else {
        String::new()
    }
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn system_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default()
}
pub fn text(locale: &str, key: &str) -> &'static str {
    static DICTIONARIES: OnceLock<[serde_json::Value; 5]> = OnceLock::new();
    let dictionaries = DICTIONARIES.get_or_init(|| {
        [
            include_str!("../../src/locales/en.json"),
            include_str!("../../src/locales/ru.json"),
            include_str!("../../src/locales/es.json"),
            include_str!("../../src/locales/zh.json"),
            include_str!("../../src/locales/hi.json"),
        ]
        .map(|s| serde_json::from_str(s).expect("valid locale file"))
    });
    let index = match locale {
        "ru" => 1,
        "es" => 2,
        "zh" => 3,
        "hi" => 4,
        _ => 0,
    };
    dictionaries[index]
        .get(key)
        .and_then(|v| v.as_str())
        .or_else(|| dictionaries[0].get(key).and_then(|v| v.as_str()))
        .unwrap_or("Pinpaper")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_variants_and_english_fallback() {
        for (input, expected) in [
            ("ru_RU.UTF-8", "ru"),
            ("es-MX", "es"),
            ("zh-Hant-TW", "zh"),
            ("hi-IN", "hi"),
            ("fr-FR", "en"),
            ("", "en"),
        ] {
            assert_eq!(resolve(input), expected);
        }
        assert_eq!(text("ru", "trayQuit"), "Выйти из Pinpaper");
    }
}
