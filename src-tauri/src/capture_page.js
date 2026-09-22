// Runs only after an explicit import request, only in the Pinterest top frame.
// Never reads inputs, passwords, cookies or localStorage. Relay data blocks
// are parsed as JSON only; page JavaScript is never evaluated.
(async () => {
  const origin = new URL(location.origin);
  if (
    window.top !== window ||
    origin.protocol !== "https:" ||
    (origin.port && origin.port !== "443") ||
    !(
      origin.hostname === "pinterest.com" ||
      origin.hostname.endsWith(".pinterest.com")
    )
  )
    return;

  const detailId =
    (location.pathname || "").match(/^\/pin\/(\d{1,32})(?:\/|$)/)?.[1] || null;
  const pins = new Map();
  const originalKey = (key) =>
    /(?:^|_)(?:orig|originals?)$/i.test(String(key || ""));
  const originalUrl = (url) => /\/originals\//i.test(url);
  // Pinterest stores the page saved with a pin in `link`. Keep only a
  // public HTTPS URL from the pin object itself; do not walk arbitrary nested
  // links because a related pin may otherwise become this pin's source.
  const sourceUrl = (value) => {
    if (!value || typeof value !== "object") return undefined;
    for (const key of [
      "link",
      "source_url",
      "sourceUrl",
      "link_url",
      "destination_url",
    ]) {
      const raw = value[key];
      if (typeof raw !== "string" || raw.length === 0 || raw.length >= 4096)
        continue;
      try {
        const url = new URL(raw);
        if (
          url.protocol !== "https:" ||
          (url.port && url.port !== "443") ||
          url.username ||
          url.password ||
          !url.hostname ||
          url.hostname === "pinterest.com" ||
          url.hostname.endsWith(".pinterest.com") ||
          url.hostname === "pinimg.com" ||
          url.hostname.endsWith(".pinimg.com")
        )
          continue;
        return url.href;
      } catch {}
    }
    return undefined;
  };
  const imageInfo = (value, key = "") => {
    if (!value || typeof value !== "object" || typeof value.url !== "string")
      return null;
    try {
      const url = new URL(value.url);
      if (
        url.protocol !== "https:" ||
        !(
          url.hostname === "i.pinimg.com" ||
          url.hostname.endsWith(".pinimg.com")
        )
      )
        return null;
      return {
        url: url.href,
        width:
          Number.isInteger(value.width) && value.width > 0 ? value.width : 0,
        height:
          Number.isInteger(value.height) && value.height > 0 ? value.height : 0,
        original: originalKey(key) || originalUrl(url.pathname),
      };
    } catch {
      return null;
    }
  };
  const assetKey = (url) => new URL(url).pathname
    .replace(/^\/(?:originals|\d+x[^/]*)\//, "/")
    .replace(/\.[^.]+$/, "");
  const cdnWidth = (url) => Number(new URL(url).pathname.match(/^\/(\d+)x/)?.[1]) || 0;
  const add = (pin) => {
    if (pins.size >= 200 || !/^\d{1,32}$/.test(pin.id)) return;
    const primary = imageInfo(
      { url: pin.url, width: pin.width, height: pin.height },
      pin.url,
    );
    if (!primary) return;
    const fallback = imageInfo(
      { url: pin.fallback_url || "" },
      pin.fallback_url,
    )?.url;
    const candidate = {
      id: pin.id,
      title: String(pin.title || "").slice(0, 300),
      description: String(pin.description || "").slice(0, 2000),
      url: primary.url,
      source_url: sourceUrl(pin),
      fallback_url: fallback && fallback !== primary.url ? fallback : undefined,
      // The legacy width/height fields are reserved for decoded pixels. Keep
      // page observations in their explicit variant fields below.
      width: 0,
      height: 0,
      original_url_exact: primary.original,
      thumbnail_width:
        Number.isInteger(pin.thumbnail_width) && pin.thumbnail_width > 0
          ? pin.thumbnail_width
          : !primary.original
            ? primary.width
            : 0,
      thumbnail_height:
        Number.isInteger(pin.thumbnail_height) && pin.thumbnail_height > 0
          ? pin.thumbnail_height
          : !primary.original
            ? primary.height
            : 0,
      max_width:
        Number.isInteger(pin.max_width) && pin.max_width > 0
          ? pin.max_width
          : 0,
      max_height:
        Number.isInteger(pin.max_height) && pin.max_height > 0
          ? pin.max_height
          : 0,
      max_dimensions_url: pin.max_dimensions_url || undefined,
    };
    const old = pins.get(pin.id);
    if (!old) {
      pins.set(pin.id, candidate);
      return;
    }
    const area = candidate.max_width * candidate.max_height;
    const oldArea = old.max_width * old.max_height;
    const candidateIsBetter =
      area > oldArea ||
      (area === oldArea &&
        candidate.original_url_exact &&
        !old.original_url_exact) ||
      (!candidate.original_url_exact && !old.original_url_exact &&
        assetKey(candidate.url) === assetKey(old.url) &&
        cdnWidth(candidate.url) > cdnWidth(old.url));
    if (candidateIsBetter) {
      pins.set(pin.id, {
        ...candidate,
        fallback_url:
          candidate.fallback_url ||
          (old.url !== candidate.url ? old.url : old.fallback_url),
        source_url: candidate.source_url || old.source_url,
      });
    } else {
      if (old.url !== candidate.url && !old.fallback_url) {
        old.fallback_url = candidate.url;
      }
      if (!old.source_url && candidate.source_url) {
        old.source_url = candidate.source_url;
      }
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
    const title = String(
      value.title || value.grid_title || value.seoTitle || "",
    );
    if (sourceUrl(value)) detail = { ...detail, source_url: sourceUrl(value) };
    const detailImages = Object.entries(value).flatMap(([key, candidate]) =>
      key === "images" && candidate && typeof candidate === "object"
        ? Object.entries(candidate)
        : [[key, candidate]],
    );
    for (const [key, candidate] of detailImages) {
      if (!originalKey(key)) continue;
      const image = imageInfo(candidate, key);
      if (image) {
        detail = {
          url: image.url,
          title: title.slice(0, 300),
          description: String(value.description || "").slice(0, 2000),
          source_url: sourceUrl(value) || detail?.source_url,
          max_width: image.original ? image.width : 0,
          max_height: image.original ? image.height : 0,
          max_dimensions_url: image.original ? image.url : undefined,
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
      if (
        /^\d{1,32}$/.test(id) &&
        !value.is_video &&
        !value.videos &&
        (!value.type || value.type === "pin")
      ) {
        const images = [];
        for (const [key, candidate] of Object.entries(value)) {
          const values =
            key === "images" && candidate && typeof candidate === "object"
              ? Object.entries(candidate).map(([nestedKey, nested]) => [
                  nestedKey,
                  nested,
                ])
              : [[key, candidate]];
          for (const [imageKey, imageCandidate] of values) {
            if (
              key !== "images" &&
              !/^(?:images?|images?_(?:orig|originals?)|imageSpec_)/i.test(
                imageKey,
              )
            )
              continue;
            const image = imageInfo(imageCandidate, imageKey);
            if (image) images.push({ key: imageKey, ...image });
          }
        }
        const originals = images
          .filter((image) => image.original)
          .sort((a, b) => b.width * b.height - a.width * a.height);
        const thumbnails = images
          .filter((image) => !image.original)
          .sort((a, b) => b.width * b.height - a.width * a.height);
        const primary = originals[0] || thumbnails[0];
        if (images.length) {
          const secondary = primary?.original
            ? thumbnails[0]
            : thumbnails[1] || originals[0];
          const max = originals.find(
            (image) => image.width > 0 && image.height > 0,
          );
          add({
            id,
            title: String(
              value.title || value.grid_title || value.seoTitle || "",
            ),
            description: String(value.description || ""),
            source_url: sourceUrl(value),
            url: primary.url,
            fallback_url: secondary?.url,
            width: primary.width,
            height: primary.height,
            thumbnail_width: thumbnails[0]?.width || 0,
            thumbnail_height: thumbnails[0]?.height || 0,
            max_width: max?.width || 0,
            max_height: max?.height || 0,
            max_dimensions_url: max?.url,
            original_url_exact: primary.original,
          });
        }
      }
    }
    for (const child of Object.values(value)) walk(child, depth + 1);
  };
  for (const s of document.querySelectorAll(
    'script[type="application/json"], script[data-relay-completed-request]',
  )) {
    if (s.textContent.length > 8000000) continue;
    try {
      let payload = s.textContent.trim();
      if (payload.startsWith("window.__PWS_RELAY_REGISTER_COMPLETED_REQUEST__(")) {
        // Pinterest's Relay scripts wrap a JSON payload in a registration call.
        // Consume the quoted request key, then parse only its JSON argument.
        const prefix = payload.match(
          /^window\.__PWS_RELAY_REGISTER_COMPLETED_REQUEST__\(\s*"(?:\\.|[^"\\])*"\s*,\s*/,
        );
        if (!prefix || !/\);?\s*$/.test(payload)) continue;
        payload = payload.slice(prefix[0].length).replace(/\);?\s*$/, "");
      }
      walk(JSON.parse(payload));
    } catch {}
  }

  if (detailId) {
    const image =
      document.querySelector('[data-test-id="pin-closeup-image"] img') ||
      document.querySelector('img[elementtiming*="closeup-image-main"]') ||
      document.querySelector('meta[property="og:image"]');
    const observedUrl =
      image?.tagName === "META"
        ? image.content
        : image?.currentSrc || image?.src || "";
    const loaded = imageInfo({
      url: observedUrl,
      width:
        image?.naturalWidth ||
        Number(
          document.querySelector('meta[property="og:image:width"]')?.content ||
            0,
        ),
      height:
        image?.naturalHeight ||
        Number(
          document.querySelector('meta[property="og:image:height"]')?.content ||
            0,
        ),
    });
    if (loaded || detail?.url) {
      const primary = detail?.url || loaded.url;
      const fallback =
        detail?.url && loaded && loaded.url !== detail.url
          ? loaded.url
          : undefined;
      const title =
        detail?.title ||
        document.querySelector('meta[property="og:title"]')?.content ||
        document.title ||
        "";
      pins.set(detailId, {
        id: detailId,
        title: title.slice(0, 300),
        description: detail?.description || "",
        url: primary,
        source_url: detail?.source_url,
        fallback_url: fallback,
        width: 0,
        height: 0,
        original_url_exact: Boolean(detail?.url),
        thumbnail_width: loaded?.width || 0,
        thumbnail_height: loaded?.height || 0,
        max_width: detail?.max_width || 0,
        max_height: detail?.max_height || 0,
        max_dimensions_url: detail?.max_dimensions_url,
      });
    }
  } else {
    // Hydrated feeds may have no structured JSON. Inspect loaded pin images only.
    for (const a of document.querySelectorAll('a[href*="/pin/"]')) {
      const id = new URL(a.href, location.href).pathname.match(
        /^\/pin\/(\d+)\//,
      )?.[1];
      const img = a.querySelector("img");
      if (!id || !img || !img.complete || !img.naturalWidth)
        continue;
      const loaded = imageInfo({
        url: img.currentSrc || img.src,
        width: img.naturalWidth || 0,
        height: img.naturalHeight || 0,
      });
      const choices = [loaded, imageInfo({ url: img.src })];
      for (const entry of (img.srcset || "").split(",")) {
        const [url, descriptor] = entry.trim().split(/\s+/);
        const variant = imageInfo({ url });
        if (variant && loaded && assetKey(variant.url) === assetKey(loaded.url)) {
          variant.rank = Number(descriptor?.replace(/w$/, "")) || 0;
          choices.push(variant);
        }
      }
      const image = choices.filter(Boolean).sort((a, b) =>
        Number(b.original) - Number(a.original) ||
        (b.rank || b.width || cdnWidth(b.url)) - (a.rank || a.width || cdnWidth(a.url)),
      )[0];
      if (image)
        add({
          id,
          title: (img.alt || "").slice(0, 300),
          description: "",
          source_url: undefined,
          url: image.url,
          fallback_url: loaded?.url !== image.url ? loaded?.url : undefined,
          width: 0,
          height: 0,
          original_url_exact: image.original,
          thumbnail_width: loaded?.width || 0,
          thumbnail_height: loaded?.height || 0,
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
