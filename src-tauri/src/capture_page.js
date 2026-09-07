// Runs only after an explicit import request, only in the Pinterest top frame.
// Never reads inputs, passwords, cookies, localStorage or arbitrary page scripts.
(async () => {
  const origin = new URL(location.origin);
  if (window.top !== window || origin.protocol !== "https:" ||
      (origin.port && origin.port !== "443") ||
      !(origin.hostname === "pinterest.com" || origin.hostname.endsWith(".pinterest.com"))) return;
  const pins = new Map();
  const add = (pin) => {
    if (pins.size >= 200 || !/^\d{1,32}$/.test(pin.id)) return;
    try {
      const u = new URL(pin.url);
      if (u.protocol !== "https:" || !u.hostname.endsWith(".pinimg.com"))
        return;
      const old = pins.get(pin.id);
      if (!old || pin.width * pin.height > old.width * old.height)
        pins.set(pin.id, pin);
    } catch {}
  };
  // Only structured JSON script data, with bounded traversal. No eval of site data.
  let visited = 0;
  const walk = (value, depth = 0) => {
    if (!value || typeof value !== "object" || depth > 30 || ++visited > 40000)
      return;
    if (
      value.id &&
      value.images &&
      !value.is_video &&
      !value.videos &&
      (!value.type || value.type === "pin")
    ) {
      for (const im of Object.values(value.images)) {
        if (im && typeof im === "object" && typeof im.url === "string")
          add({
            id: String(value.id),
            title: String(value.title || value.grid_title || "").slice(0, 300),
            description: String(value.description || "").slice(0, 2000),
            url: im.url,
            width: Number.isInteger(im.width) && im.width > 0 ? im.width : 0,
            height:
              Number.isInteger(im.height) && im.height > 0 ? im.height : 0,
          });
      }
    }
    for (const child of Object.values(value)) walk(child, depth + 1);
  };
  for (const s of document.querySelectorAll(
    'script[type="application/json"]',
  )) {
    if (s.textContent.length > 8000000) continue;
    try {
      walk(JSON.parse(s.textContent));
    } catch {}
  }
  // Hydrated feeds may have no structured JSON. Inspect loaded pin images only.
  for (const a of document.querySelectorAll('a[href*="/pin/"]')) {
    const id = new URL(a.href, location.href).pathname.match(
      /^\/pin\/(\d+)\//,
    )?.[1];
    const img = a.querySelector("img");
    if (!id || !img || !img.complete || !img.naturalWidth || pins.has(id))
      continue;
    try {
      const u = new URL(img.currentSrc || img.src);
      // Pinterest thumbnails often have an original counterpart. Dimensions remain
      // unknown until the native downloader validates the actual image.
      u.pathname = u.pathname.replace(
        /^\/(?:\d+x(?:\d+)?(?:_[^/]+)?)\//,
        "/originals/",
      );
      add({
        id,
        title: (img.alt || "").slice(0, 300),
        description: "",
        url: u.href,
        width: 0,
        height: 0,
      });
    } catch {}
  }
  await window.__TAURI_INTERNALS__.invoke("browser_report", {
    report: {
      nonce: __PINPAPER_NONCE__,
      page_url: location.href,
      pins: [...pins.values()],
      error: null,
    },
  });
})();
