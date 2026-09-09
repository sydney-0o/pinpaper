use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub interval_minutes: u64,
    pub active_start: u32,
    pub active_end: u32,
    pub enabled: bool,
    pub keywords: String,
    pub exclude: String,
    pub orientation: String,
    pub min_width: u32,
    pub board_ids: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_minutes: 60,
            active_start: 8,
            active_end: 23,
            enabled: false,
            keywords: String::new(),
            exclude: String::new(),
            orientation: "landscape".into(),
            min_width: 1280,
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
#[derive(Clone, Serialize, Deserialize)]
pub struct Pin {
    #[serde(default)]
    pub dimensions_verified: bool,
    pub id: String,
    pub board_id: String,
    pub title: String,
    pub description: String,
    pub url: String,
    /// An image URL observed on Pinterest and kept as a fallback when the
    /// preferred observed URL is no longer available from the CDN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_url: Option<String>,
    pub width: u32,
    pub height: u32,
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
pub fn ranked(lib: &Library) -> Vec<Pin> {
    let wanted = words(&lib.settings.keywords);
    let excluded = words(&lib.settings.exclude);
    let mut results: Vec<(f64, Pin)> = lib
        .pins
        .iter()
        .filter(|p| {
            let tokens = words(&format!("{} {}", p.title, p.description));
            lib.settings.board_ids.contains(&p.board_id)
                && lib.feedback.get(&p.id) != Some(&-1)
                && lib.current.as_ref().map(|c| c.id != p.id).unwrap_or(true)
                && ((p.board_id == crate::browser_session::SOURCE
                    && (!p.dimensions_verified || (p.width == 0 && p.height == 0)))
                    || (p.width >= lib.settings.min_width
                        && p.height > 0
                        && match lib.settings.orientation.as_str() {
                            "landscape" => p.width > p.height,
                            "portrait" => p.height > p.width,
                            _ => true,
                        }))
                && !excluded.iter().any(|w| tokens.contains(w))
        })
        .map(|p| {
            let tokens = words(&format!("{} {}", p.title, p.description));
            let score = wanted.iter().filter(|w| tokens.contains(w)).count() as f64 * 4.0
                - if lib.history.contains(&p.id) {
                    100.0
                } else {
                    0.0
                };
            (score, p.clone())
        })
        .collect();
    results.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    results.into_iter().map(|(_, p)| p).collect()
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
    fn ranking_respects_feedback_filters_and_history() {
        let mut l = Library::default();
        l.settings.board_ids = vec!["b".into()];
        l.settings.keywords = "forest".into();
        l.pins = vec![pin("1", "ocean"), pin("2", "forest"), pin("3", "forest")];
        l.feedback.insert("3".into(), -1);
        assert_eq!(ranked(&l)[0].id, "2");
        assert_eq!(ranked(&l).len(), 2);
        l.history.push("2".into());
        assert_eq!(ranked(&l)[0].id, "1");
        l.settings.exclude = "forest".into();
        assert_eq!(ranked(&l).len(), 1);
        l.settings.min_width = 4000;
        assert!(ranked(&l).is_empty());
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
}
