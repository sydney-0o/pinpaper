fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "snapshot",
            "save_settings",
            "next_wallpaper",
            "feedback",
            "disconnect",
            "open_pin",
            "set_pin_hidden",
            "browser_open",
            "browser_close",
            "browser_import",
            "browser_report",
        ]),
    ))
    .expect("Tauri build failed");
}
