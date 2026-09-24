<div align="center">

<img src="docs/readme/hero.svg" alt="Pinpaper — Your Pinterest pictures, on your desktop." width="100%">

<p>
  <a href="#english">English</a>
  ·
  <a href="#russian">Русский</a>
</p>

</div>

<a id="english"></a>

## English

Import pictures from Pinterest and change your desktop wallpaper manually or on a schedule.

Runs in the menu bar or system tray on macOS, Windows, and Linux. Your imported collection stays on your computer.

### What Pinpaper does

- Import pictures from Pinterest Home or a saved board.
- Filter by collections, words, orientation, and minimum width.
- Change the wallpaper manually or on a schedule.
- Hide pictures locally, open their pins, and launch Pinpaper with your computer.

### Screenshots

<table align="center">
  <tr>
    <td valign="top"><img src="docs/readme/gallery/home-collection.png" alt="Pinpaper collection with a yellow flower wallpaper" width="320"></td>
    <td valign="top"><img src="docs/readme/gallery/home-forest.png" alt="Pinpaper collection with a forest wallpaper" width="320"></td>
  </tr>
  <tr>
    <td valign="top"><img src="docs/readme/gallery/home-roses.png" alt="Pinpaper collection with a rose wallpaper" width="320"></td>
    <td valign="top"><img src="docs/readme/gallery/home-flowers.png" alt="Pinpaper collection with a flowering tree wallpaper" width="320"></td>
  </tr>
</table>

<p align="center"><sub>A compact view of your current wallpaper, schedule, and collection.</sub></p>

### Install

Builds are coming to [Releases](https://github.com/sydney-0o/pinpaper/releases). Download the file for your system and open it when the first build is available.

| System | File | What to do |
| --- | --- | --- |
| macOS 11+ | `.dmg` | Open the disk image and drag Pinpaper to Applications. |
| Windows 10/11 | `x64-portable.exe` | Keep it in any folder and double-click it. |
| Linux | `.AppImage` | Make the file executable and launch it. |
| Debian / Ubuntu | `.deb` | Open the package with the system software installer. |

<details>
<summary>macOS details</summary>

Open the `.dmg`, drag Pinpaper to Applications, and launch it from there. An unsigned build may show a macOS warning; control-click the app, choose **Open**, and confirm when that happens.

</details>

<details>
<summary>Windows details</summary>

`x64-portable.exe` does not install Pinpaper system-wide: keep it in any folder and run it there. Windows 10 and 11 normally include WebView2. If Windows reports that WebView2 is missing, install the [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) once.

An unsigned build may also trigger SmartScreen. Before choosing **More info → Run anyway**, check that the file came from this repository's Releases page.

</details>

<details>
<summary>Linux details</summary>

For an AppImage, enable **Allow executing file as program** in the file properties and open it. The equivalent terminal commands are:

```sh
chmod +x Pinpaper_*_linux.AppImage
./Pinpaper_*_linux.AppImage
```

Use the `.deb` package on Debian or Ubuntu. AppImage is convenient when you want a movable copy without an installation step.

The wallpaper adapter supports GNOME, Unity, Budgie, Cinnamon, and MATE. KDE, Xfce, and wlroots-only desktops are not supported yet.

</details>

### First launch

1. Open Pinpaper and go to **Settings**.
2. Choose **Open Pinterest** and sign in in the private Pinterest window that Pinpaper opens. Complete any verification there.
3. In that window, open Home or **Profile → Saved → a board**, scroll, and wait for the pictures you want to load.
4. Return to Pinpaper and choose **Add pictures**. Each import can add up to 200 loaded pictures; scroll further and repeat when needed.
5. Select collections and filters, save the settings, and choose **Change wallpaper**.

Under **Automatic changes**, set an interval and active hours, then enable automatic changes. Closing the main window leaves Pinpaper in the menu bar or system tray; choose **Quit Pinpaper** there to stop it completely.

#### Start with your computer

Enable **Launch Pinpaper when I sign in** in Settings and save. Clear the checkbox to turn it off. If you use a portable AppImage, keep it in a permanent folder before enabling startup.

<details>
<summary>Startup file locations</summary>

| System | Per-user startup entry |
| --- | --- |
| macOS | `~/Library/LaunchAgents/app.pinpaper.desktop.plist` |
| Windows | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
| Linux | `~/.config/autostart/app.pinpaper.desktop` |

The entry is created for the current user only and does not require administrator access. For an AppImage, Pinpaper stores the path to that AppImage.

</details>

<details>
<summary>How wallpaper rotation moves through a collection</summary>

Rotation checks the remaining eligible pictures until the current round is exhausted. A failed download or an image that is too small counts as checked in that round, including a failed hand-off from a public outbound source back to the observed Pinterest copy. When filters change, the remaining eligible tail is kept; once the active eligible set is exhausted, a new round starts. Pictures already shown successfully stay in the saved progress, so a restart or an update does not require deleting the collection or importing it again.

</details>

### Pinterest, sources, and privacy

Pinpaper respects Pinterest and is not affiliated with, sponsored by, or endorsed by Pinterest. Pinterest is a trademark of Pinterest, Inc.

Pinpaper **does not use or exploit the Pinterest API**. No API key or developer registration is needed. You open Pinterest yourself in Pinpaper's private window, sign in there, and choose the page to import. Pinpaper never reads password fields, and the private window session is not stored after you close it.

Imported metadata, preferences, and the local image cache stay on your computer. Opening a pin or hiding a picture changes only Pinpaper's local behavior. When a pin contains a verifiable public source page, the app may use that image; if the source is unavailable or cannot be verified, Pinpaper stays with the observed Pinterest copy and does not invent a URL.

Pinterest pages can change, require verification, or expose only part of a feed. Videos are skipped. If the pictures you want were not loaded, scroll further in Pinterest and repeat the import.

Image rights remain with their authors and owners. Pinpaper helps you use your personal collection on your desktop and does not claim ownership of the images.

### License

Pinpaper is distributed under the [GNU GPLv3](LICENSE). The complete license text is in [`LICENSE`](LICENSE).

For a problem or improvement, open an [issue](https://github.com/sydney-0o/pinpaper/issues).

<details>
<summary>Build from source and release workflow</summary>

Pinpaper is a Tauri 2 desktop application with a React/TypeScript interface and a Rust core. Local development needs Node.js 22+, stable Rust, and the native dependencies for the target OS.

```sh
npm ci
npm run tauri -- dev
```

Useful local checks are:

```sh
npm test
npm run build
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

Build packages on the target operating system:

```sh
# macOS
npm run tauri -- build --bundles dmg

# Linux
npm run tauri -- build --bundles appimage,deb

# Windows portable executable
npm run tauri -- build --no-bundle
```

Windows builds use WebView2 and Visual Studio C++ Build Tools. Linux builds need WebKitGTK 4.1, GTK, AppIndicator, `libxdo`, OpenSSL, and the usual build tools. GitHub Actions builds on the target operating system and publishes tagged releases.

| System | Release file |
| --- | --- |
| macOS | `Pinpaper_<version>_macOS.dmg` |
| Windows | `Pinpaper_<version>_x64-portable.exe` |
| Linux | `Pinpaper_<version>_linux.AppImage` and `Pinpaper_<version>_linux.deb` |

</details>

<hr>

<a id="russian"></a>

## Русский

Импортируйте картинки из Pinterest и меняйте обои рабочего стола вручную или по расписанию.

Работает в меню-баре или системном трее на macOS, Windows и Linux. Импортированная коллекция остаётся на вашем компьютере.

### Что умеет Pinpaper

- Импортировать картинки из Home или сохранённой доски Pinterest.
- Фильтровать их по коллекциям, словам, ориентации и минимальной ширине.
- Менять обои вручную или по расписанию.
- Локально скрывать картинки, открывать их пины и запускаться вместе с компьютером.

Галерея интерфейса показана выше один раз, чтобы английская и русская версии не дублировали изображения.

### Установка

Готовые сборки появятся на странице [Releases](https://github.com/sydney-0o/pinpaper/releases). Когда появится первая сборка, скачайте файл своей системы и откройте его.

| Система | Файл | Что делать |
| --- | --- | --- |
| macOS 11+ | `.dmg` | Открыть образ и перетащить Pinpaper в Applications. |
| Windows 10/11 | `x64-portable.exe` | Сохранить в удобную папку и запустить двойным щелчком. |
| Linux | `.AppImage` | Сделать файл исполняемым и запустить. |
| Debian / Ubuntu | `.deb` | Открыть пакет системным установщиком программ. |

<details>
<summary>Подробности для macOS</summary>

Откройте `.dmg`, перетащите Pinpaper в Applications и запустите его оттуда. Неподписанная сборка может показать предупреждение macOS: нажмите по приложению правой кнопкой, выберите **Open** и подтвердите запуск.

</details>

<details>
<summary>Подробности для Windows</summary>

`x64-portable.exe` не устанавливает Pinpaper в систему: его можно хранить и запускать из любой папки. Windows 10 и 11 обычно уже содержат WebView2. Если Windows сообщит, что WebView2 отсутствует, один раз установите [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).

Неподписанный файл может вызвать предупреждение SmartScreen. Перед запуском проверьте, что файл скачан со страницы Releases этого репозитория.

</details>

<details>
<summary>Подробности для Linux</summary>

Для AppImage откройте свойства файла, включите **Allow executing file as program** и запустите его. В терминале это выглядит так:

```sh
chmod +x Pinpaper_*_linux.AppImage
./Pinpaper_*_linux.AppImage
```

DEB подходит для Debian и Ubuntu. AppImage удобнее, если нужна переносимая копия без установки.

Поддержаны интеграции обоев GNOME, Unity, Budgie, Cinnamon и MATE. KDE, Xfce и окружения только с wlroots пока не поддерживаются.

</details>

### Первый запуск

1. Откройте Pinpaper и перейдите в **Настройки**.
2. Нажмите **Открыть Pinterest** и войдите в открывшемся приватном окне Pinterest. Если появится проверка, пройдите её там же.
3. В этом окне откройте Home или **Профиль → Сохранённые → доска**, прокрутите страницу и дождитесь загрузки нужных картинок.
4. Вернитесь в Pinpaper и нажмите **Добавить картинки**. За один импорт можно добавить до 200 загруженных картинок; при необходимости прокрутите Pinterest дальше и повторите импорт.
5. Выберите коллекции и фильтры, сохраните настройки и нажмите **Сменить обои**.

В разделе **Автоматическая смена** задайте интервал и часы активности, затем включите автоматическую смену. Закрытие окна оставляет Pinpaper работать в меню-баре или системном трее; пункт **Выйти из Pinpaper** полностью останавливает приложение.

#### Запуск вместе с компьютером

Включите **Запускать Pinpaper при входе в систему** в настройках и сохраните их. Снимите галочку, чтобы отключить автозапуск. Для переносимого AppImage сначала положите файл в постоянную папку.

<details>
<summary>Технические пути автозапуска</summary>

| Система | Запись для текущего пользователя |
| --- | --- |
| macOS | `~/Library/LaunchAgents/app.pinpaper.desktop.plist` |
| Windows | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
| Linux | `~/.config/autostart/app.pinpaper.desktop` |

Запись создаётся только для текущего пользователя и не требует прав администратора. Для AppImage Pinpaper сохраняет путь к самому файлу.

</details>

<details>
<summary>Как Pinpaper проходит коллекцию</summary>

Ротация проверяет оставшиеся подходящие картинки до конца текущего круга. Неудачная загрузка или изображение, которое оказалось слишком маленьким, считается проверенным в этом круге — в том числе если приложение перешло с внешнего источника на наблюдаемую копию Pinterest. После изменения фильтров оставшийся подходящий хвост сохраняется; новый круг начинается только после исчерпания активного набора. Уже успешно показанные картинки остаются в сохранённом прогрессе, поэтому после перезапуска или обновления не нужно удалять коллекцию и импортировать её заново.

</details>

### Pinterest, источники и приватность

Pinpaper уважает Pinterest, не связан с Pinterest, не спонсируется им и не является его официальным приложением. Pinterest — товарный знак Pinterest, Inc.

Pinpaper **не использует и не эксплуатирует Pinterest API**. Для импорта не нужны ключ API или регистрация разработчика. Вы сами открываете Pinterest в приватном окне Pinpaper, сами проходите вход и сами выбираете страницу для импорта. Pinpaper не читает поля пароля, а сессия приватного окна не сохраняется после его закрытия.

Метаданные, настройки и локальный кэш хранятся на вашем компьютере. Открытие пина и скрытие картинки меняют только поведение Pinpaper. Когда у пина есть проверяемая публичная страница-источник, приложение может использовать изображение с неё; если источник недоступен или не подтверждён, Pinpaper остаётся на наблюдаемой копии Pinterest и не придумывает URL.

Pinterest может изменить страницу, показать проверку или загрузить только часть ленты. Видео пропускаются. Если нужные картинки не попали в импорт, прокрутите Pinterest дальше и повторите импорт.

Права на изображения остаются у их авторов и владельцев. Pinpaper помогает использовать вашу личную подборку на рабочем столе и не заявляет права на сами изображения.

### Лицензия

Pinpaper распространяется по лицензии [GNU GPLv3](LICENSE). Полный текст лицензии находится в файле [`LICENSE`](LICENSE).

Если нашли проблему или хотите предложить улучшение, создайте [issue](https://github.com/sydney-0o/pinpaper/issues).

<details>
<summary>Сборка из исходников и выпуск релизов</summary>

Pinpaper — приложение Tauri 2 с интерфейсом на React/TypeScript и ядром на Rust. Для локальной разработки понадобятся Node.js 22+, stable Rust и системные зависимости целевой ОС.

```sh
npm ci
npm run tauri -- dev
```

Локальные проверки:

```sh
npm test
npm run build
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

Сборка пакетов выполняется на соответствующей системе:

```sh
# macOS
npm run tauri -- build --bundles dmg

# Linux
npm run tauri -- build --bundles appimage,deb

# Windows portable executable
npm run tauri -- build --no-bundle
```

Сборка Windows использует WebView2 и Visual Studio C++ Build Tools. Для Linux нужны WebKitGTK 4.1, GTK, AppIndicator, `libxdo`, OpenSSL и обычные инструменты сборки. GitHub Actions собирает приложение на целевой системе и публикует tagged-релизы.

| Система | Файл релиза |
| --- | --- |
| macOS | `Pinpaper_<version>_macOS.dmg` |
| Windows | `Pinpaper_<version>_x64-portable.exe` |
| Linux | `Pinpaper_<version>_linux.AppImage` и `Pinpaper_<version>_linux.deb` |

</details>
