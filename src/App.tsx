import { useEffect, useMemo, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowUpRight,
  Check,
  ChevronLeft,
  ChevronRight,
  Clock3,
  EyeOff,
  Images,
  Leaf,
  LoaderCircle,
  Pause,
  Play,
  Plus,
  Settings2,
  SkipForward,
  Square,
  Undo2,
} from "lucide-react";
import { resolveLanguage, translator } from "./i18n";
type Settings = {
  interval_minutes: number;
  active_start: number;
  active_end: number;
  enabled: boolean;
  launch_at_login: boolean;
  keywords: string;
  exclude: string;
  orientation: string;
  min_width: number;
  display_mode: string;
  board_ids: string[];
};
type Pin = {
  id: string;
  board_id: string;
  title: string;
  description: string;
  url: string;
  width: number;
  height: number;
};
type Snapshot = {
  library: {
    settings: Settings;
    boards: { id: string; name: string }[];
    pins: Pin[];
    feedback: Record<string, number>;
    history: string[];
    current: Pin | null;
    last_change: number;
    last_sync: number;
  };
  total_pictures: number;
  selected_pictures: number;
  available_pictures: number;
  unknown_resolution_pictures: number;
  hidden_pictures: number;
  connected: boolean;
  browser_connected: boolean;
  browser_open: boolean;
  busy: boolean;
  changing: boolean;
  change_status: {
    phase: "searching" | "retrying" | "applying";
    attempt: number;
    max_attempts: number;
  } | null;
  error: string | null;
  preview: string | null;
  locale: string;
};
const WIDTH_OPTIONS = [
  0, 720, 1080, 1280, 1366, 1440, 1600, 1920, 2048, 2560, 2880, 3000, 3200,
  3440, 3840, 4096, 5120, 6016, 7680,
];
const defaults: Settings = {
  interval_minutes: 60,
  active_start: 8,
  active_end: 23,
  enabled: false,
  launch_at_login: false,
  keywords: "",
  exclude: "",
  orientation: "landscape",
  min_width: 1280,
  display_mode: "fill",
  board_ids: [],
};
const initial: Snapshot = {
  library: {
    settings: defaults,
    boards: [],
    pins: [],
    feedback: {},
    history: [],
    current: null,
    last_change: 0,
    last_sync: 0,
  },
  connected: false,
  browser_connected: false,
  browser_open: false,
  busy: false,
  changing: false,
  change_status: null,
  error: null,
  preview: null,
  locale: resolveLanguage(navigator.languages),
  total_pictures: 0,
  selected_pictures: 0,
  available_pictures: 0,
  unknown_resolution_pictures: 0,
  hidden_pictures: 0,
};
function initialState(): Snapshot {
  // Synthetic, development-only browser review. No real account, file or native action.
  const query = new URLSearchParams(location.search);
  if (
    import.meta.env.DEV &&
    !isTauri() &&
    query.get("review") === "connected"
  ) {
    const pins = Array.from({ length: 30 }, (_, i) => ({
      id: String(123 + i),
      board_id: "browser-session",
      title: `Mountain view ${i + 1}`,
      description: "",
      url: "",
      width: 1920,
      height: 1080,
    }));
    return {
      ...initial,
      locale: resolveLanguage([query.get("lang") || initial.locale]),
      connected: true,
      browser_connected: true,
      browser_open: true,
      library: {
        ...initial.library,
        current: pins[0],
        pins,
        boards: [{ id: "browser-session", name: "Browser collection" }],
        settings: { ...defaults, board_ids: ["browser-session"] },
      },
      total_pictures: pins.length,
      selected_pictures: pins.length,
      available_pictures: pins.length - 1,
      unknown_resolution_pictures: 0,
      hidden_pictures: 0,
    };
  }
  return import.meta.env.DEV && !isTauri()
    ? {
        ...initial,
        locale: resolveLanguage([query.get("lang") || initial.locale]),
      }
    : initial;
}
export default function App() {
  const [state, setState] = useState(initialState);
  const [page, setPage] = useState<"home" | "settings" | "collection">("home");
  const [draft, setDraft] = useState(defaults);
  const [busy, setBusy] = useState(false),
    [operation, setOperation] = useState(""),
    [error, setError] = useState<string | null>(null),
    [notice, setNotice] = useState("");
  const [resetPending, setResetPending] = useState(false);
  const [step, setStep] = useState(0),
    [source, setSource] = useState<"home" | "saved">("home");
  const [search, setSearch] = useState(""),
    [collectionSource, setCollectionSource] = useState("all"),
    [showHidden, setShowHidden] = useState(false),
    [collectionPage, setCollectionPage] = useState(0);
  const refreshing = useRef<Promise<Snapshot> | null>(null),
    refreshAgain = useRef(false);
  const desktop = isTauri(),
    t = translator(state.locale),
    blocked = busy || state.busy;
  const { library } = state,
    current = library.current,
    enabled = library.settings.enabled;
  const n = (value: number) =>
    new Intl.NumberFormat(state.locale).format(value);
  const currentIndex = current
    ? library.pins.findIndex(
        (pin) => pin.id === current.id && pin.board_id === current.board_id,
      )
    : -1;
  async function refresh() {
    if (!desktop) return state;
    if (refreshing.current) refreshAgain.current = true;
    else
      refreshing.current = (async () => {
        let latest: Snapshot;
        do {
          refreshAgain.current = false;
          latest = await invoke<Snapshot>("snapshot");
          setState(latest);
        } while (refreshAgain.current);
        return latest;
      })().finally(() => {
        refreshing.current = null;
      });
    return refreshing.current;
  }
  useEffect(() => {
    const update = () => {
      refresh().catch((e) => setError(String(e)));
    };
    const visible = () => {
      if (!document.hidden) update();
    };
    let disposed = false,
      unlisten: (() => void) | undefined;
    if (desktop) {
      listen("pinpaper-changed", update)
        .then((stop) => {
          if (disposed) stop();
          else {
            unlisten = stop;
            update();
          }
        })
        .catch((e) => setError(String(e)));
      window.addEventListener("focus", update);
      document.addEventListener("visibilitychange", visible);
    }
    return () => {
      disposed = true;
      unlisten?.();
      window.removeEventListener("focus", update);
      document.removeEventListener("visibilitychange", visible);
    };
  }, []);
  useEffect(() => {
    document.documentElement.lang = state.locale;
  }, [state.locale]);
  async function action(command: string, args?: Record<string, unknown>) {
    if (!desktop) {
      setError("preview");
      return false;
    }
    setBusy(true);
    setOperation(command);
    setError(null);
    setNotice("");
    try {
      await invoke(command, args);
      await refresh();
      return true;
    } catch (e) {
      setError(String(e));
      await refresh().catch(() => {});
      return false;
    } finally {
      setBusy(false);
    }
  }
  const sourceName = (id: string) =>
    id === "browser-session"
      ? t("browserCollection")
      : library.boards.find((b) => b.id === id)?.name || t("otherPictures");
  const filtered = useMemo(
    () =>
      library.pins.filter(
        (pin) =>
          (collectionSource === "all" || pin.board_id === collectionSource) &&
          (showHidden || library.feedback[pin.id] !== -1) &&
          `${pin.title} ${pin.description} ${pin.id}`
            .toLocaleLowerCase()
            .includes(search.toLocaleLowerCase().trim()),
      ),
    [library.pins, library.feedback, collectionSource, showHidden, search],
  );
  const pages = Math.max(1, Math.ceil(filtered.length / 24)),
    currentPage = Math.min(collectionPage, pages - 1),
    pictures = filtered.slice(currentPage * 24, (currentPage + 1) * 24);
  const selected = library.pins.filter(
    (pin) =>
      draft.board_ids.includes(pin.board_id) && library.feedback[pin.id] !== -1,
  ).length;
  useEffect(() => {
    setCollectionPage(0);
  }, [search, collectionSource, showHidden]);
  useEffect(() => {
    window.scrollTo({ top: 0, behavior: "auto" });
  }, [page, currentPage]);
  useEffect(() => {
    if (error || state.error || notice)
      document.querySelector(".message")?.scrollIntoView({ block: "nearest" });
  }, [error, state.error, notice]);
  function settings() {
    setDraft({ ...library.settings });
    setStep(state.browser_open ? (library.pins.length ? 2 : 1) : 0);
    setPage("settings");
    setNotice("");
  }
  function update<K extends keyof Settings>(key: K, value: Settings[K]) {
    setDraft((d) => ({ ...d, [key]: value }));
  }
  const problem = error || state.error;
  const canRetrySearch =
    problem?.includes("Wallpaper search paused") ||
    problem?.includes("No suitable wallpapers");
  const errorHint = problem?.includes("403")
    ? t("errorForbidden")
    : problem === "preview"
      ? t("previewOnly")
      : problem?.includes("Reset completed except image cache cleanup failed")
        ? t("errorResetCache")
        : problem?.includes("Wallpaper search paused") ||
            problem?.includes("No suitable wallpapers")
          ? t("retrySearch")
          : problem?.includes("Wallpaper change stopped")
            ? t("changeStopped")
          : problem?.includes("No Pinterest page") ||
              problem?.includes("sign-in") ||
              problem?.includes("signing in")
            ? t("errorNoPage")
            : problem?.includes("No image pins") ||
                problem?.includes("did not answer")
              ? t("errorNoPins")
              : problem?.includes("No matching") || problem?.includes("filters")
                ? t("errorFilters")
                : t("retryHelp");
  const hasSelection =
    library.settings.board_ids.length > 0 && library.pins.length > 0;
  const changeStatus = state.change_status;
  const changeLabel =
    changeStatus?.phase === "retrying"
      ? t("searchingNext", {
          attempt: n(changeStatus.attempt),
          max: n(changeStatus.max_attempts),
        })
      : changeStatus?.phase === "applying"
        ? t("searchingApply")
      : changeStatus?.phase === "searching"
          ? t("searchingFirst", {
              attempt: n(changeStatus.attempt),
              max: n(changeStatus.max_attempts),
            })
          : t("changing");
  return (
    <main className={`page-${page}`}>
      <header>
        <div className="brand">
          <Leaf size={22} />
          <span>
            pinpaper<span className="brand-dot">.</span>
          </span>
        </div>
        <span className="brand-tag">{t("brandTag")}</span>
      </header>
      {!desktop && <div className="preview-banner">{t("previewOnly")}</div>}
      {problem && (
        <div role="alert" className="message error">
          <strong>{t("errorTitle")}</strong>
          <p>{errorHint}</p>
          {canRetrySearch && hasSelection && (
            <button
              className="secondary error-retry"
              disabled={blocked}
              onClick={() => action("next_wallpaper")}
            >
              <SkipForward size={16} />
              {t("retryNow")}
            </button>
          )}
          {problem !== "preview" && (
            <details>
              <summary>{t("details")}</summary>
              <code>{problem}</code>
            </details>
          )}
        </div>
      )}
      {notice && (
        <div role="status" className="message success">
          {notice}
        </div>
      )}
      {page === "home" ? (
        <>
          <div className="section-heading">
            <span className="eyebrow">
              {t(current ? "current" : "welcomeLabel")}
            </span>
            <button className="secondary settings-button" onClick={settings}>
              <Settings2 size={17} />
              {t("settings")}
            </button>
          </div>
          <div className={"art " + (state.preview ? "has-image" : "")}>
            {state.preview ? (
              <img src={state.preview} alt={current?.title || t("current")} />
            ) : (
              <>
                <div className="sun" />
                <div className="hill hill-back" />
                <div className="hill hill-front" />
              </>
            )}
            {current && (
              <button
                className="source-link"
                title={t("openOriginal")}
                aria-label={t("openOriginal")}
                onClick={() => action("open_pin")}
              >
                <ArrowUpRight size={18} />
              </button>
            )}
          </div>
          <div className="caption">
            <h1>{current?.title || t("welcome")}</h1>
            {current && (
              <div className="picture-meta">
                {currentIndex >= 0 && (
                  <span className="picture-position" title={t("picturePositionHint")}>
                    {t("picturePosition", {
                      index: n(currentIndex + 1),
                      total: n(library.pins.length),
                    })}
                  </span>
                )}
                {current.width > 0 && current.height > 0 && (
                  <span className="dimensions">
                    {current.width} × {current.height}
                  </span>
                )}
              </div>
            )}
          </div>
          {!library.pins.length && <p className="intro">{t("getStarted")}</p>}
          {!library.pins.length ? (
            <button className="primary full" onClick={settings}>
              <Plus size={18} />
              {t("addPictures")}
            </button>
          ) : (
            <div className="actions">
              <button
                className="primary"
                disabled={blocked || !hasSelection}
                aria-busy={
                  state.changing ||
                  (busy && ["next_wallpaper", "feedback"].includes(operation))
                }
                onClick={() => action("next_wallpaper")}
              >
                {state.changing ||
                (busy && ["next_wallpaper", "feedback"].includes(operation)) ? (
                  <>
                    <LoaderCircle className="spin" size={19} />
                    <span role="status">{changeLabel}</span>
                  </>
                ) : (
                  <>
                    <SkipForward size={17} />
                    {t("changeWallpaper")}
                  </>
                )}
              </button>
              <button
                className="secondary"
                disabled={blocked || !current}
                title={t("hideHint")}
                onClick={() => action("feedback", { value: -1 })}
              >
                <EyeOff size={18} />
                {t("hide")}
              </button>
            </div>
          )}
          {!!library.pins.length && !hasSelection && (
            <p className="hint">{t("selectedEmpty")}</p>
          )}
          <section className="schedule-card">
            <div className="row">
              <div className="row-label">
                <span className={"schedule-icon " + (enabled ? "active" : "")}>
                  <Clock3 size={19} />
                </span>
                <div>
                  <strong>{t(enabled ? "autoOn" : "autoOff")}</strong>
                  <small>
                    {t("everyMinutes", {
                      minutes: n(library.settings.interval_minutes),
                    })}
                  </small>
                </div>
              </div>
              <button
                className="icon toggle"
                disabled={blocked || (!enabled && !hasSelection)}
                onClick={() =>
                  action("save_settings", {
                    settings: { ...library.settings, enabled: !enabled },
                  })
                }
              >
                {enabled ? <Pause size={17} /> : <Play size={17} />}{" "}
                {t(enabled ? "pause" : "start")}
              </button>
            </div>
            <div className="card-footer">
              <span>
                {library.settings.active_start === library.settings.active_end
                  ? t("allDay")
                  : `${String(library.settings.active_start).padStart(2, "0")}:00 — ${String(library.settings.active_end).padStart(2, "0")}:00`}{" "}
                · {t("localTime")}
              </span>
              <button className="text-button" onClick={settings}>
                {t("editSchedule")}
              </button>
            </div>
          </section>
          <button className="library-row" onClick={() => setPage("collection")}>
            <span>
              <Images size={19} />
              {t("browsePictures", { count: n(library.pins.length) })}
            </span>
            <ChevronRight size={18} />
          </button>
        </>
      ) : page === "collection" ? (
        <>
          <div className="preferences-title">
            <button className="secondary" onClick={() => setPage("home")}>
              <ChevronLeft size={18} />
              {t("back")}
            </button>
            <h1>{t("yourPictures")}</h1>
          </div>
          <p className="intro">{t("galleryHint")}</p>
          <label>
            {t("search")}
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={t("searchHint")}
            />
          </label>
          <label>
            {t("collection")}
            <select
              value={collectionSource}
              onChange={(e) => setCollectionSource(e.target.value)}
            >
              <option value="all">{t("allCollections")}</option>
              {[...new Set(library.pins.map((pin) => pin.board_id))].map(
                (id) => (
                  <option key={id} value={id}>
                    {sourceName(id)}
                  </option>
                ),
              )}
            </select>
          </label>
          <label className="board">
            <input
              type="checkbox"
              checked={showHidden}
              onChange={(e) => setShowHidden(e.target.checked)}
            />
            {t("showHidden")}
          </label>
          <p role="status" className="hint">
            {t("pageCount", {
              count: n(filtered.length),
              page: n(currentPage + 1),
              pages: n(pages),
            })}
          </p>
          <div className="picture-list">
            {pictures.map((pin) => {
              const hidden = library.feedback[pin.id] === -1;
              return (
                <article
                  className="picture-item"
                  key={`${pin.board_id}:${pin.id}`}
                >
                  <div className="picture-item-head">
                    <div className="picture-item-meta">
                      <h2>{pin.title || t("untitled")}</h2>
                      <p className="hint">
                        {sourceName(pin.board_id)}
                        {hidden
                          ? ` · ${t("hidden")}`
                          : !library.settings.board_ids.includes(pin.board_id)
                            ? ` · ${t("notSelected")}`
                            : ""}
                        {current?.id === pin.id ? ` · ${t("current")}` : ""}
                      </p>
                    </div>
                    <button
                      className="secondary picture-item-action"
                      disabled={blocked}
                      title={t(hidden ? "useAgain" : "hideFromWallpapers")}
                      aria-label={t(hidden ? "useAgain" : "hideFromWallpapers")}
                      onClick={() =>
                        action("set_pin_hidden", {
                          pinId: pin.id,
                          hidden: !hidden,
                        })
                      }
                    >
                      {hidden ? <Undo2 size={17} /> : <EyeOff size={17} />}
                    </button>
                  </div>
                  <button
                    className="picture-link"
                    onClick={() => action("open_pin", { pinId: pin.id })}
                  >
                    {t("openOriginal")} <ArrowUpRight size={16} />
                  </button>
                </article>
              );
            })}
          </div>
          {!pictures.length && (
            <p className="empty">
              {t(library.pins.length ? "noMatches" : "noPictures")}
            </p>
          )}
          {pages > 1 && (
            <div className="pagination">
              <button
                className="secondary"
                disabled={currentPage === 0}
                onClick={() => setCollectionPage(currentPage - 1)}
              >
                {t("previous")}
              </button>
              <button
                className="secondary"
                disabled={currentPage >= pages - 1}
                onClick={() => setCollectionPage(currentPage + 1)}
              >
                {t("nextPage")}
              </button>
            </div>
          )}
          <button className="primary full" onClick={settings}>
            {t("chooseAdd")}
          </button>
        </>
      ) : (
        <>
          <div className="preferences-title">
            <button className="secondary" onClick={() => setPage("home")}>
              <ChevronLeft size={18} />
              {t("back")}
            </button>
            <h1>{t("settings")}</h1>
          </div>
          <section className="form-section setup-section">
            <h2>{t("connectTitle")}</h2>
            <div
              className="setup-steps"
              role="group"
              aria-label={t("connectTitle")}
            >
              {(["stepSignIn", "stepLoad", "stepAdd"] as const).map(
                (key, index) => (
                  <button
                    key={key}
                    className={step === index ? "step active" : "step"}
                    aria-pressed={step === index}
                    onClick={() => setStep(index)}
                  >
                    <span className="step-number" aria-hidden="true">
                      {index + 1}
                    </span>
                    <span>{t(key)}</span>
                  </button>
                ),
              )}
            </div>
            <div className="step-panel">
              {step === 0 ? (
                <>
                  <p>{t("signInGuide")}</p>
                  <button
                    className="primary full"
                    disabled={blocked}
                    onClick={async () => {
                      if (await action("browser_open")) setStep(1);
                    }}
                  >
                    <ArrowUpRight size={17} />
                    {t(
                      state.browser_open ? "returnPinterest" : "openPinterest",
                    )}
                  </button>
                  <p className="hint">{t("privateHint")}</p>
                </>
              ) : step === 1 ? (
                <>
                  <label>
                    {t("pictureSource")}
                    <select
                      value={source}
                      onChange={(e) =>
                        setSource(e.target.value as "home" | "saved")
                      }
                    >
                      <option value="home">{t("homeFeed")}</option>
                      <option value="saved">{t("savedPins")}</option>
                    </select>
                  </label>
                  <p>{t(source === "home" ? "homeGuide" : "savedGuide")}</p>
                  <button
                    className="secondary full"
                    disabled={blocked}
                    onClick={() => action("browser_open")}
                  >
                    {t(
                      state.browser_open ? "returnPinterest" : "openPinterest",
                    )}
                  </button>
                  <button
                    className="primary full"
                    disabled={!state.browser_open}
                    onClick={() => setStep(2)}
                  >
                    {t("loadedReady")}
                  </button>
                </>
              ) : (
                <>
                  <p>{t("addGuide")}</p>
                  <button
                    className="primary full"
                    disabled={blocked || !state.browser_open}
                    onClick={async () => {
                      if (await action("browser_import")) {
                        const latest = await refresh();
                        setDraft((d) => ({
                          ...d,
                          board_ids: latest.library.settings.board_ids,
                        }));
                        setNotice(t("addedNotice"));
                      }
                    }}
                  >
                    {t("addPictures")}
                  </button>
                  {!state.browser_open && (
                    <p className="hint">{t("openFirst")}</p>
                  )}
                  <p role="status" className="hint">
                    {t("addedCount", { count: n(library.pins.length) })}
                  </p>
                </>
              )}
            </div>
          </section>
          <section className="form-section">
            <h2>{t("collections")}</h2>
            <p>{t("collectionsHint")}</p>
            {library.boards.length ? (
              <div className="boards">
                {library.boards.map((board) => (
                  <label className="board collection-option" key={board.id}>
                    <input
                      type="checkbox"
                      checked={draft.board_ids.includes(board.id)}
                      onChange={(e) =>
                        update(
                          "board_ids",
                          e.target.checked
                            ? [...draft.board_ids, board.id]
                            : draft.board_ids.filter((id) => id !== board.id),
                        )
                      }
                    />
                    <span>{sourceName(board.id)}</span>
                    <span className="collection-count">
                      {t("available", {
                        count: n(
                          library.pins.filter(
                            (pin) =>
                              pin.board_id === board.id &&
                              library.feedback[pin.id] !== -1,
                          ).length,
                        ),
                      })}
                    </span>
                  </label>
                ))}
              </div>
            ) : (
              <p className="empty">{t("noPictures")}</p>
            )}
            <p role="status" className="hint">
              {t("selectionCount", { count: n(selected) })}
              {!draft.board_ids.length ? ` ${t("noCollections")}` : ""}
            </p>
            <details className="collection-details">
              <summary>{t("collectionDetails")}</summary>
              <p role="status" className="hint">
                {t("pictureCounts", {
                  total: n(state.total_pictures),
                  selected: n(state.selected_pictures),
                  available: n(state.available_pictures),
                  unknown: n(state.unknown_resolution_pictures),
                  hidden: n(state.hidden_pictures),
                })}
              </p>
            </details>
            <div className="collection-actions">
              <button
                className="primary full"
                disabled={blocked || !library.boards.length}
                onClick={async () => {
                  if (
                    await action("save_settings", {
                      settings: {
                        ...library.settings,
                        board_ids: draft.board_ids,
                      },
                    })
                  )
                    setNotice(t("selectionApplied"));
                }}
              >
                {t("applySelection")}
              </button>
              <button
                className="secondary full"
                onClick={() => setPage("collection")}
              >
                {t("browseSaved")}
              </button>
            </div>
            <button
              className="secondary full use-all"
              disabled={blocked || !library.pins.length}
              onClick={() => {
                const boardIds = [
                  ...new Set(
                    library.pins
                      .filter((pin) => library.feedback[pin.id] !== -1)
                      .map((pin) => pin.board_id),
                  ),
                ];
                setDraft((d) => ({
                  ...d,
                  board_ids: boardIds,
                  keywords: "",
                  exclude: "",
                  orientation: "any",
                  min_width: 0,
                }));
                setNotice(t("allPicturesPrepared"));
              }}
            >
              {t("useAllPictures")}
            </button>
            <p className="field-help">{t("useAllPicturesHint")}</p>
          </section>
          <section className="form-section">
            <h2>{t("preferences")}</h2>
            <label>
              {t("preferWords")}
              <input
                value={draft.keywords}
                onChange={(e) => update("keywords", e.target.value)}
                placeholder={t("preferExample")}
              />
            </label>
            <label>
              {t("avoidWords")}
              <input
                value={draft.exclude}
                onChange={(e) => update("exclude", e.target.value)}
                placeholder={t("avoidExample")}
              />
            </label>
            <p className="hint">{t("wordsHint")}</p>
            <div className="split">
              <label>
                {t("shape")}
                <select
                  value={draft.orientation}
                  onChange={(e) => update("orientation", e.target.value)}
                >
                  <option value="landscape">{t("wide")}</option>
                  <option value="portrait">{t("portrait")}</option>
                  <option value="any">{t("any")}</option>
                </select>
              </label>
              <label>
                {t("minWidth")}
                <select
                  value={draft.min_width}
                  onChange={(e) => update("min_width", Number(e.target.value))}
                >
                  {WIDTH_OPTIONS.map((width) => (
                    <option value={width} key={width}>
                      {width ? `${n(width)} px` : t("anySize")}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <p className="field-help resolution-help">{t("minWidthHelp")}</p>
          </section>
          <section className="form-section">
            <h2>{t("autoTitle")}</h2>
            <label>
              {t("changeEvery")}
              <select
                value={draft.interval_minutes}
                onChange={(e) =>
                  update("interval_minutes", Number(e.target.value))
                }
              >
                {[1, 15, 30, 60, 120, 360, 720, 1440].map((minutes) => (
                  <option key={minutes} value={minutes}>
                    {t(minutes < 60 ? "minutes" : "hours", {
                      count: n(minutes < 60 ? minutes : minutes / 60),
                    })}
                  </option>
                ))}
              </select>
            </label>
            <div className="split">
              {(["active_start", "active_end"] as const).map((key, index) => (
                <label key={key}>
                  {t(index ? "stopAt" : "startAt")}
                  <select
                    value={draft[key]}
                    onChange={(e) => update(key, Number(e.target.value))}
                  >
                    {Array.from({ length: 24 }, (_, hour) => (
                      <option key={hour} value={hour}>
                        {String(hour).padStart(2, "0")}:00
                      </option>
                    ))}
                  </select>
                </label>
              ))}
            </div>
            <p className="hint">{t("scheduleHint")}</p>
            <label className="board">
              <input
                type="checkbox"
                checked={draft.enabled}
                onChange={(e) => update("enabled", e.target.checked)}
              />
              {t("autoEnable")}
            </label>
          </section>
          <section className="form-section">
            <h2>{t("startupTitle")}</h2>
            <label className="board">
              <input
                type="checkbox"
                checked={draft.launch_at_login}
                onChange={(e) => update("launch_at_login", e.target.checked)}
              />
              {t("launchAtLogin")}
            </label>
            <p className="field-help">{t("launchAtLoginHint")}</p>
          </section>
          <details className="account-section">
            <summary>{t("account")}</summary>
            <p className="hint">{t("accountHint")}</p>
            <button
              className="secondary full"
              disabled={blocked || !state.browser_open}
              onClick={async () => {
                if (await action("browser_close")) setStep(0);
              }}
            >
              {t("signOut")}
            </button>
            <button
              className="text-button danger"
              disabled={blocked}
              onClick={() => setResetPending(true)}
            >
              {t("reset")}
            </button>
            {resetPending && (
              <div className="reset-confirm" role="alert">
                <p>{t("resetConfirm")}</p>
                <div className="reset-confirm-actions">
                  <button
                    className="secondary"
                    disabled={blocked}
                    onClick={() => setResetPending(false)}
                  >
                    {t("cancelReset")}
                  </button>
                  <button
                    className="text-button danger"
                    disabled={blocked}
                    onClick={async () => {
                      setResetPending(false);
                      if (await action("disconnect")) {
                        setDraft({ ...defaults });
                        setStep(0);
                        setNotice(t("resetDone"));
                      }
                    }}
                  >
                    {t("confirmReset")}
                  </button>
                </div>
              </div>
            )}
          </details>
          <div className="save-bar">
            <button
              className="primary full"
              disabled={blocked}
              onClick={async () => {
                if (await action("save_settings", { settings: draft }))
                  setNotice(t("settingsSaved"));
              }}
            >
              <Check size={16} />
              {t("saveSettings")}
            </button>
          </div>
        </>
      )}
      {blocked && (
        <div className="working" role="status">
          <span className="working-label">
            <LoaderCircle className="spin" size={15} />
            {t(
              operation === "browser_import"
                ? "adding"
                : operation === "next_wallpaper"
                  ? "changing"
                  : operation === "stop_wallpaper_change"
                    ? "stopping"
                    : "working",
            )}
          </span>
          {state.changing && (
            <button
              className="secondary stop-change"
              disabled={busy && operation === "stop_wallpaper_change"}
              onClick={() => action("stop_wallpaper_change")}
            >
              <Square size={14} />
              {t("stopChange")}
            </button>
          )}
        </div>
      )}
      <footer className="help-footer">
        <details>
          <summary>{t("helpTitle")}</summary>
          <p>{t("retryHelp")}</p>
          <button
            className="text-button"
            onClick={() => {
              settings();
              setStep(0);
            }}
          >
            {t("retry")}
          </button>
        </details>
      </footer>
    </main>
  );
}
