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
fn thumbnail_path(path: &Path) -> PathBuf {
    path.with_extension("preview.jpg")
}
pub fn prepare(path: &Path, image: &image::DynamicImage) -> Option<()> {
    let thumb = image.thumbnail(560, 360).to_rgb8();
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 75)
        .encode_image(&image::DynamicImage::ImageRgb8(thumb))
        .ok()?;
    let target = thumbnail_path(path);
    let temp = target.with_extension("tmp");
    std::fs::write(&temp, bytes).ok()?;
    std::fs::rename(temp, target).ok()
}
pub fn load(path: &Path) -> Option<String> {
    use base64::Engine;
    let target = thumbnail_path(path);
    let bytes = match std::fs::read(&target) {
        Ok(bytes) => bytes,
        Err(_) => {
            let image = image::open(path).ok()?;
            prepare(path, &image)?;
            std::fs::read(target).ok()?
        }
    };
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
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
        let prepare_start = std::time::Instant::now();
        prepare(&path, &image::DynamicImage::ImageRgb8(image)).unwrap();
        println!(
            "prepare thumbnail from decoded image: {:?}",
            prepare_start.elapsed()
        );
        // A prepared thumbnail must work even without reopening the full-size file.
        std::fs::remove_file(&path).unwrap();
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
