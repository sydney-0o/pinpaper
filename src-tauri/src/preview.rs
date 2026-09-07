use std::path::{Path, PathBuf};

/// Holds one thumbnail, including a failed lookup, until the wallpaper changes.
#[derive(Default)]
pub struct PreviewCache {
    entry: Option<(PathBuf, Option<String>)>,
}
impl PreviewCache {
    pub fn clear(&mut self) {
        self.entry = None;
    }
    pub fn get_or_load(
        &mut self,
        path: &Path,
        load: impl FnOnce(&Path) -> Option<String>,
    ) -> Option<String> {
        if self.entry.as_ref().map(|e| e.0.as_path()) != Some(path) {
            self.entry = Some((path.to_owned(), load(path)));
        }
        self.entry.as_ref().and_then(|e| e.1.clone())
    }
}
pub fn load(path: &Path) -> Option<String> {
    use base64::Engine;
    let im = image::open(path).ok()?.thumbnail(760, 500);
    let mut bytes = std::io::Cursor::new(vec![]);
    im.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_refresh_does_not_decode_again_and_change_invalidates() {
        let mut cache = PreviewCache::default();
        let mut calls = 0;
        for _ in 0..100 {
            assert_eq!(
                cache.get_or_load(Path::new("one"), |_| {
                    calls += 1;
                    Some("thumbnail".into())
                }),
                Some("thumbnail".into())
            );
        }
        assert_eq!(calls, 1);
        assert_eq!(cache.get_or_load(Path::new("two"), |_| None), None);
        assert_eq!(
            cache.get_or_load(Path::new("two"), |_| panic!("missing image retried")),
            None
        );
        cache.clear();
        assert_eq!(
            cache.get_or_load(Path::new("two"), |_| Some("new".into())),
            Some("new".into())
        );
    }
}

#[cfg(test)]
mod benchmark {
    use super::*;
    #[test]
    #[ignore = "explicit synthetic performance check; never changes the desktop"]
    fn large_image_refresh_benchmark() {
        let dir =
            std::env::temp_dir().join(format!("pinpaper-preview-benchmark-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("synthetic.jpg");
        let image = image::RgbImage::from_fn(3840, 2160, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
        });
        image.save(&path).unwrap();
        let mut cache = PreviewCache::default();
        let start = std::time::Instant::now();
        let first = cache.get_or_load(&path, load).unwrap();
        let first_time = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            let result = cache.get_or_load(&path, |_| panic!("decoded again"));
            assert_eq!(result.as_ref().unwrap().len(), first.len());
        }
        println!(
            "3840x2160 preview: first decode {:?}; 1000 cached reads {:?}; cached bytes {}",
            first_time,
            start.elapsed(),
            first.len()
        );
    }
}
