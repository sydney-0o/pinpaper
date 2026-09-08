# Pinpaper

A small Pinterest wallpaper companion built with Tauri 2, Rust, React and TypeScript. Closing the main window keeps rotation running in the tray.

## Use

Settings presents three steps, one at a time:

1. **Sign in:** open Pinterest and complete login and any verification.
2. **Load pictures:** for recommendations, open Home and scroll down until pictures appear. Scroll further to load more. For saved pins, open your profile → Saved → a board, then scroll through the individual pins (board covers alone are not enough). Leave the Pinterest window open and return to Pinpaper.
3. **Add pictures:** import the loaded pictures and check the collection count. Return to the main screen and choose Change wallpaper.

Adding pictures immediately saves the import. Apply selection and Save settings are only needed after changing those controls; a deliberately disabled collection stays disabled.

In **Pictures to use**, select collections and apply the selection. Browse pictures opens a searchable, paginated list with Pinterest links. Hide excludes a picture locally; hidden pictures can be restored. Nothing is liked, deleted or changed on Pinterest. Like was removed because it did not send a Pinterest like.

Choose picture filters and a schedule, then save settings. Preferred words influence local ranking; excluded words, orientation, minimum width and collection selection filter candidates. Ranking uses titles/descriptions, not image recognition. Start/Pause controls automatic changes.

The interface follows the system language: English, Russian, Spanish, simplified Chinese or Hindi. Unsupported languages use English. Restart after changing the system language. Account actions are separated from picture selection. If something fails, use the footer to repeat the steps; error details remain available separately.

## Pinterest connection and limits

No developer account, app secret, OAuth/API setup or credential-store access is required. The official API path has been removed. This is a manual import from the Pinterest page you open, not an official recommendation API or a background crawler.

The private Pinterest window keeps authentication only while open. Closing it requires signing in again; imported pictures and settings remain. Sign out closes Pinterest without clearing the collection. Reset removes local Pinpaper data. Legacy credentials from older versions are ignored, not read or deleted.

Each import captures up to 200 loaded image pins, retaining up to 1,000. Repeat the steps to add more. Videos are skipped. Pinterest page changes, verification and webview restrictions can affect login/import. Regional subdomains of pinterest.com are accepted consistently by navigation, capture and reporting; import stays bound to the same selected window. This addresses a mismatch behind “No Pinterest page found”, but real-account acceptance of this update remains necessary.

Only app-owned Pinterest windows receive the restricted report permission. Reports are nonce-bound and time-limited; image URLs and dimensions are validated. Password fields are never read. Verification popups preserve their opener configuration; at most four are allowed.

## Performance and storage

The interface refreshes on changes and focus instead of polling every five seconds. Preview decoding runs off the UI thread and is cached until the wallpaper changes. Collection browsing uses 24 link rows per page without automatically downloading thumbnails. These changes remove repeated image work associated with freezing; live CPU profiling of the user's running instance was not performed.

A single event-driven worker prepares up to two upcoming candidates in the background after startup, imports, settings or wallpaper changes. Rapid requests are coalesced and candidates re-ranked between downloads; prefetch never changes history or applies a wallpaper. Foreground and background share one writer per URL, and completed files are reused. Background errors stay quiet with a five-minute retry cooldown; manual changes can retry immediately. There is no prefetch polling or unbounded download queue. The Change wallpaper button shows a spinner while a foreground change runs.

Images download on demand or by bounded prefetch over HTTPS from pinimg.com, with redirects refused, a 25 MiB download limit and a persistent disk image cache. Decoding limits dimensions to 16,384 pixels per axis and allocation to 256 MiB. Metadata, preferences, hidden flags and recent history live in library.json under Tauri's app-data directory for app.pinpaper.desktop. This metadata is not encrypted. On macOS the usual locations are ~/Library/Application Support/app.pinpaper.desktop and ~/Library/Caches/app.pinpaper.desktop.

The scheduler checks every 15 seconds. Active hours use local time, support overnight ranges and treat equal endpoints as all day. Failed automatic changes wait five minutes before retrying. After sleep, at most one overdue change runs. Manual changes ignore active hours. Launch at login is not implemented. Only explicit imports add new pins.

## Platforms

| Platform | Adapter and limits |
| --- | --- |
| macOS | Native NSWorkspace on connected screens; no System Events Automation requirement. On each Space switch, the current cached wallpaper is reapplied to connected screens. Inactive Spaces update when visited while Pinpaper runs; enable macOS “Show on all Spaces” for system-wide mirroring. |
| Windows | SystemParametersInfoW; one wallpaper using existing OS fit behavior. |
| Linux GNOME / Unity / Budgie | gsettings light and dark wallpaper URIs; matching schema required. |
| Linux Cinnamon / MATE | Desktop-specific gsettings keys. |
| KDE / Xfce / wlroots-only desktops | Not supported yet. |

The tray uses a leaf: a template icon on macOS and a green icon elsewhere.

## Run and build

Install Node.js 22+, stable Rust and the platform prerequisites for Tauri 2: Xcode Command Line Tools on macOS; Visual Studio C++ Build Tools, Windows SDK and WebView2 on Windows; WebKitGTK 4.1, GTK and AppIndicator development libraries on Linux. GNOME may need an AppIndicator extension.

```sh
npm ci
npm run tauri -- dev
```

For the optional project-local Rust installation on this development machine:

```sh
export RUSTUP_HOME="$PWD/work/toolchain/rustup"
export CARGO_HOME="$PWD/work/toolchain/cargo"
export PATH="$CARGO_HOME/bin:$PATH"
```

```sh
npm test
cargo test --locked --manifest-path src-tauri/Cargo.toml
npm run tauri -- build --no-bundle
```

Omit --no-bundle to generate installers/bundles under src-tauri/target/release/bundle. Build on each target OS. Distribution signing/notarization must be configured separately. The no-bundle build leaves existing app bundles untouched.

npm run dev starts an interface-only browser preview. Development-only ?review=connected&lang=ru supplies synthetic pictures for layout review; desktop actions are unavailable there.

## Validation

The current macOS release build and TypeScript/Vite build passed. Rust: 21 tests passed, one optional benchmark ignored. Node: seven tests passed, covering extraction, regional origins, all translation keys/placeholders and language fallback. A separately run synthetic preview benchmark measured approximately 57 ms for an initial 3840×2160 decode and 10 ms total for 1,000 cached reads. These are synthetic measurements, not live application profiling.

The running user instance was not closed, replaced or driven. Earlier browser layout checks covered 380×560 and 460×760; the final translated layout still needs full visual acceptance. The three-OS workflow in .github/workflows/build.yml is prepared but has not run remotely. Windows/Linux compilation and desktop behavior have not been verified here.

Manual acceptance should cover login and verification, scrolling Home and saved boards, repeated imports, source selection, hiding/restoring pictures, narrow-window layout in each language, sign-out/relogin, scheduling across sleep and active hours, tray actions and connected displays.

## Source map

- src/: interface, translations and styles.
- src-tauri/src/main.rs: lifecycle, commands, scheduler and persistence.
- browser_session.rs and capture_page.js: private browser and bounded imports.
- model.rs: settings and local ranking.
- preview.rs: cached preview generation.
- wallpaper.rs: bounded downloads and platform adapters.
- language.rs: system language and native labels.
- assets/leaf.svg and scripts/render-tray.py: tray artwork and reproducible rendering (Pillow).

Downloaded images, including pictures rejected by current size filters, remain on disk until local data is reset. There is no automatic cache eviction. Change wallpaper checks all ranked candidates until one downloads and matches the actual resolution; there is no five-candidate cutoff. OS wallpaper-setting errors still stop immediately.

Image quality: downloads request the original Pinterest size path first, including previously imported thumbnails. Only HTTP 404/410 falls back to the supplied source. Unverified metadata dimensions do not exclude originals. EXIF rotation is applied before checking dimensions. Images are cached as lossless PNG without resizing or another JPEG compression pass. Existing JPEGs remain readable for the current wallpaper; new changes and prefetch use a separate cache version. This cannot repair blur already present in the source. A 3440x1440 fill requires at least 3440 pixels wide and 1440 high to avoid enlargement.

Performance update: wallpaper downloads prepare a separate 560x360 JPEG preview at quality 75 from the already-decoded pixels. Later snapshots read that small file rather than decoding the full wallpaper again. Full-size PNG uses fast lossless compression. A synthetic 3840x2160 release benchmark measured about 24 ms to create a preview, 0.10 ms to read it, and 1.7 ms for 1,000 memory-cache reads; this does not measure network or live NSWorkspace latency. Redundant native wallpaper updates and a duplicate successful library write are skipped.
