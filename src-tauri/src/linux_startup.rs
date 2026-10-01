//! Linux startup guards for WebKitGTK's renderer process.
//!
//! WebKitGTK can abort while creating a GBM EGL display on some NVIDIA
//! driver/compositor combinations.  WebKitGTK has two relevant environment
//! switches: older builds expose the broad DMABUF switch, while newer builds
//! also expose the narrower GBM switch.  Select the switch from the loaded
//! WebKitGTK runtime version so an AppImage's bundled WebKit and a DEB's host
//! WebKit receive the compatible guard.  This trades some renderer
//! acceleration for a safer display path on affected Linux starts, but does
//! not disable unrelated GPU or display backends.

#[cfg(target_os = "linux")]
const WEBKIT_DISABLE_DMABUF_RENDERER: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
#[cfg(target_os = "linux")]
const WEBKIT_DMABUF_RENDERER_DISABLE_GBM: &str = "WEBKIT_DMABUF_RENDERER_DISABLE_GBM";
const ENABLED: &str = "1";
const NARROW_GBM_MIN_VERSION: (u32, u32, u32) = (2, 42, 0);

/// The GBM-only escape hatch is present by the upstream 2.41.91 GTK build;
/// 2.42 is its first stable series. Keep the broad switch for older WebKitGTK
/// runtimes, where the GBM-specific switch may be ignored.
fn supports_gbm_only_guard(runtime_version: (u32, u32, u32)) -> bool {
    runtime_version >= NARROW_GBM_MIN_VERSION
}

/// Return defaults only when neither supported escape hatch was user-set.
///
/// Keeping this policy pure lets the tests cover the default and override
/// cases without mutating the process environment in parallel test runs.
fn default_values(
    runtime_version: (u32, u32, u32),
    dmabuf_already_set: bool,
    gbm_already_set: bool,
) -> [Option<&'static str>; 2] {
    if dmabuf_already_set || gbm_already_set {
        [None, None]
    } else if supports_gbm_only_guard(runtime_version) {
        [None, Some(ENABLED)]
    } else {
        [Some(ENABLED), None]
    }
}

/// Configure WebKitGTK before Tauri/GTK can start any renderer threads.
#[cfg(target_os = "linux")]
pub fn configure() {
    let runtime_version = runtime_version();
    let [dmabuf_value, gbm_value] = default_values(
        runtime_version,
        std::env::var_os(WEBKIT_DISABLE_DMABUF_RENDERER).is_some(),
        std::env::var_os(WEBKIT_DMABUF_RENDERER_DISABLE_GBM).is_some(),
    );
    if let Some(value) = dmabuf_value {
        std::env::set_var(WEBKIT_DISABLE_DMABUF_RENDERER, value);
    }
    if let Some(value) = gbm_value {
        std::env::set_var(WEBKIT_DMABUF_RENDERER_DISABLE_GBM, value);
    }
}

#[cfg(target_os = "linux")]
fn runtime_version() -> (u32, u32, u32) {
    // SAFETY: These WebKitGTK API functions only return the version of the
    // already-loaded shared library. They do not initialize GTK/WebKit or
    // start threads, so this call remains safe before Tauri is constructed.
    unsafe {
        (
            webkit2gtk::ffi::webkit_get_major_version(),
            webkit2gtk::ffi::webkit_get_minor_version(),
            webkit2gtk::ffi::webkit_get_micro_version(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{default_values, supports_gbm_only_guard};

    #[test]
    fn uses_broad_fallback_for_older_webkit() {
        assert_eq!(default_values((2, 41, 6), false, false), [Some("1"), None]);
    }

    #[test]
    fn uses_broad_fallback_until_stable_2_42() {
        assert_eq!(default_values((2, 41, 91), false, false), [Some("1"), None]);
    }

    #[test]
    fn uses_gbm_only_fallback_at_supported_boundary() {
        assert!(supports_gbm_only_guard((2, 42, 0)));
        assert_eq!(default_values((2, 42, 0), false, false), [None, Some("1")]);
    }

    #[test]
    fn uses_gbm_only_fallback_for_noble_webkit() {
        assert_eq!(default_values((2, 44, 0), false, false), [None, Some("1")]);
    }

    #[test]
    fn uses_gbm_only_fallback_for_modern_webkit() {
        assert_eq!(default_values((2, 52, 6), false, false), [None, Some("1")]);
    }

    #[test]
    fn preserves_explicit_dmabuf_override() {
        assert_eq!(default_values((2, 52, 6), true, false), [None, None]);
    }

    #[test]
    fn preserves_explicit_gbm_override() {
        assert_eq!(default_values((2, 41, 6), false, true), [None, None]);
    }

    #[test]
    fn preserves_both_explicit_overrides() {
        assert_eq!(default_values((2, 41, 6), true, true), [None, None]);
    }
}
