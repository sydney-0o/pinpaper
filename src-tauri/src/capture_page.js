// Runs only after an explicit import request, only in the Pinterest top frame.
// Never reads inputs, passwords, cookies, localStorage or arbitrary page scripts.
(async () => {
  const origin = new URL(location.origin);
  if (window.top !== window || origin.protocol !== "https:" ||
      (origin.port && origin.port !== "443") ||
      !(origin.hostname === "pinterest.com" || origin.hostname.endsWith(".pinterest.com"))) return;

  const detailId = (location.pathname || "").match(/^\/pin\/(\d{1,32})(?:\/|$)/)?.[1] || null;
  const pins = new Map();
  const imageInfo = (value) => {
    if (!value || typeof value !== "object" || typeof value.url !== "string")
      return null;
    try {
      const url = new URL(value.url);
      if (url.protocol !== "https:" ||
          !(url.hostname === "i.pinimg.com" || url.hostname.endsWith(".pinimg.com")))
        return null;
      return {
        url: url.href,
        width: Number.isInteger(value.width) && value.width > 0 ? value.width : 0,
        height: Number.isInteger(value.height) && value.height > 0 ? value.height : 0,
      };
    } catch {
      return null;
    }
  };
  const add = (pin) => {
    if (pins.size >= 200 || !/^\d{1,32}$/.test(pin.id)) return;
    const primary = imageInfo({ url: pin.url, width: pin.width, height: pin.height });
    if (!primary) return;
    const fallback = imageInfo({ url: pin.fallback_url || "" })?.url;
    const candidate = {
      id: pin.id,
      title: String(pin.title || "").slice(0, 300),
      description: String(pin.description || "").slice(0, 2000),
      url: primary.url,
      fallback_url: fallback && fallback !== primary.url ? fallback : undefined,
      width: primary.width,
      height: primary.height,
    };
    const old = pins.get(pin.id);
    if (!old) {
      pins.set(pin.id, candidate);
      return;
    }
    const area = candidate.width * candidate.height;
    const oldArea = old.width * old.height;
    if (area > oldArea) {
      pins.set(pin.id, {
        ...candidate,
        fallback_url:
          candidate.fallback_url ||
          (old.url !== candidate.url ? old.url : old.fallback_url),
      });
    } else if (old.url !== candidate.url && !old.fallback_url) {
      old.fallback_url = candidate.url;
    }
  };

  // A pin detail page stores the original URL under images_orig while the
  // rendered image is often a smaller, working URL. Keep both exact URLs.
  let detail = null;
  let visited = 0;
  const inspectDetail = (value) => {
    if (!detailId || !value || typeof value !== "object") return;
    const id = String(value.entityId || value.id || "");
    if (id !== detailId) return;
    const title = String(value.title || value.grid_title || value.seoTitle || "");
    for (const [key, candidate] of Object.entries(value)) {
      if (!/(?:^|_)(?:orig|originals?)$/i.test(key)) continue;
      const image = imageInfo(candidate);
      if (image) {
        detail = {
          url: image.url,
          title: title.slice(0, 300),
          description: String(value.description || "").slice(0, 2000),
        };
        return;
      }
    }
  };
  const walk = (value, depth = 0) => {
    if (!value || typeof value !== "object" || depth > 30 || ++visited > 40000)
      return;
    if (detailId) {
      inspectDetail(value);
    } else {
      const id = String(value.entityId || value.id || "");
      if (/^\d{1,32}$/.test(id) && !value.is_video && !value.videos &&
          (!value.type || value.type === "pin")) {
        const images = [];
        for (const [key, candidate] of Object.entries(value)) {
          const values = key === "images" && candidate && typeof candidate === "object"
            ? Object.entries(candidate).map(([nestedKey, nested]) => [nestedKey, nested])
            : [[key, candidate]];
          for (const [imageKey, imageCandidate] of values) {
            if (key !== "images" && !/^(?:images?|imageSpec_)/i.test(imageKey)) continue;
            const image = imageInfo(imageCandidate);
            if (image) images.push({ key: imageKey, ...image });
          }
        }
        images.sort((a, b) => {
          const rank = (image) =>
            image.width * image.height || (/orig/i.test(image.key) ? 1e15 : 0);
          return rank(b) - rank(a);
        });
        if (images.length) {
          const [primary, secondary] = images;
          add({
            id,
            title: String(value.title || value.grid_title || value.seoTitle || ""),
            description: String(value.description || ""),
            url: primary.url,
            fallback_url: secondary?.url,
            width: primary.width,
            height: primary.height,
          });
        }
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

  if (detailId) {
    const image =
      document.querySelector('[data-test-id="pin-closeup-image"] img') ||
      document.querySelector('img[elementtiming*="closeup-image-main"]') ||
      document.querySelector('meta[property="og:image"]');
    const observedUrl = image?.tagName === "META"
      ? image.content
      : image?.currentSrc || image?.src || "";
    const loaded = imageInfo({
      url: observedUrl,
      width: image?.naturalWidth || Number(
        document.querySelector('meta[property="og:image:width"]')?.content || 0,
      ),
      height: image?.naturalHeight || Number(
        document.querySelector('meta[property="og:image:height"]')?.content || 0,
      ),
    });
    if (loaded || detail?.url) {
      const primary = detail?.url || loaded.url;
      const fallback = detail?.url && loaded && loaded.url !== detail.url
        ? loaded.url
        : undefined;
      const title = detail?.title ||
        document.querySelector('meta[property="og:title"]')?.content ||
        document.title || "";
      pins.set(detailId, {
        id: detailId,
        title: title.slice(0, 300),
        description: detail?.description || "",
        url: primary,
        fallback_url: fallback,
        width: loaded?.width || 0,
        height: loaded?.height || 0,
      });
    }
  } else {
    // Hydrated feeds may have no structured JSON. Inspect loaded pin images only.
    for (const a of document.querySelectorAll('a[href*="/pin/"]')) {
      const id = new URL(a.href, location.href).pathname.match(
        /^\/pin\/(\d+)\//,
      )?.[1];
      const img = a.querySelector("img");
      if (!id || !img || !img.complete || !img.naturalWidth || pins.has(id))
        continue;
      const image = imageInfo({
        url: img.currentSrc || img.src,
        // A loaded feed image is normally a thumbnail; verify its final
        // dimensions only after the native downloader decodes it.
        width: 0,
        height: 0,
      });
      if (image)
        add({
          id,
          title: (img.alt || "").slice(0, 300),
          description: "",
          url: image.url,
          width: image.width,
          height: image.height,
        });
    }
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
