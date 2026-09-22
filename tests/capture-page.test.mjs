import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
const script = readFileSync(
  new URL("../src-tauri/src/capture_page.js", import.meta.url),
  "utf8",
).replace("__PINPAPER_NONCE__", '"fixture-nonce"');
async function capture({
  json = [],
  relay = [],
  anchors = [],
  origin = "https://www.pinterest.com",
  path = "/",
  detailImage,
  detailTitle = "",
  child = false,
} = {}) {
  let output;
  const window = {
    __TAURI_INTERNALS__: {
      invoke: async (command, args) => {
        assert.equal(command, "browser_report");
        output = args.report;
      },
    },
  };
  window.top = child ? {} : window;
  const document = {
    querySelectorAll: (selector) =>
      selector.startsWith("script")
        ? [
            ...json.map((v) => ({ textContent: JSON.stringify(v) })),
            ...relay.map((v) => ({ textContent: typeof v === "string" ? v :
              `window.__PWS_RELAY_REGISTER_COMPLETED_REQUEST__("%7Brequest%7D",${JSON.stringify(v)});` })),
          ]
        : anchors,
    querySelector: (selector) => {
      if (!path.startsWith("/pin/")) return null;
      if (
        selector.includes("pin-closeup-image") ||
        selector.includes("closeup-image-main")
      )
        return detailImage ? { ...detailImage, tagName: "IMG" } : null;
      if (selector.includes("og:image:width")) return { content: "736" };
      if (selector.includes("og:image:height")) return { content: "1307" };
      if (selector.includes("og:image"))
        return { content: detailImage?.src || "" };
      if (selector.includes("og:title")) return { content: detailTitle };
      return null;
    },
  };
  Object.defineProperty(document, "cookie", {
    get() {
      throw new Error("Cookie access forbidden");
    },
  });
  await vm.runInNewContext(script, {
    window,
    document,
    location: { origin, href: origin + path, pathname: path },
    documentTitle: detailTitle,
    URL,
    Map,
  });
  return output;
}
test("structured data chooses largest image, deduplicates and excludes video", async () => {
  const result = await capture({
    json: [
      {
        pins: [
          {
            id: "123",
            type: "pin",
            title: "Forest",
            link: "https://wallhaven.cc/w/zpvp3g",
            images: {
              small: {
                url: "https://i.pinimg.com/236x/a.jpg",
                width: 236,
                height: 120,
              },
              orig: {
                url: "https://i.pinimg.com/originals/a.jpg",
                width: 2000,
                height: 1000,
              },
            },
          },
          {
            id: "456",
            type: "pin",
            is_video: true,
            images: {
              orig: {
                url: "https://i.pinimg.com/v.jpg",
                width: 2000,
                height: 1000,
              },
            },
          },
        ],
      },
    ],
  });
  assert.equal(result.nonce, "fixture-nonce");
  assert.equal(result.pins.length, 1);
  assert.equal(result.pins[0].width, 0);
  assert.equal(result.pins[0].original_url_exact, true);
  assert.equal(result.pins[0].max_width, 2000);
  assert.equal(result.pins[0].max_height, 1000);
  assert.equal(
    result.pins[0].source_url,
    "https://wallhaven.cc/w/zpvp3g",
  );
  assert.equal(
    result.pins[0].max_dimensions_url,
    "https://i.pinimg.com/originals/a.jpg",
  );
});
test("duplicate structured records retain an observed original without dimensions", async () => {
  const result = await capture({
    json: [
      {
        id: "987",
        images: {
          small: { url: "https://i.pinimg.com/736x/987.jpg" },
        },
      },
      {
        id: "987",
        images: {
          orig: { url: "https://i.pinimg.com/originals/987.png" },
        },
      },
    ],
  });
  assert.equal(result.pins.length, 1);
  assert.equal(result.pins[0].url, "https://i.pinimg.com/originals/987.png");
  assert.equal(result.pins[0].original_url_exact, true);
  assert.equal(
    result.pins[0].fallback_url,
    "https://i.pinimg.com/736x/987.jpg",
  );
});
test("loaded DOM pins preserve a fallback source without inventing dimensions", async () => {
  const result = await capture({
    anchors: [
      {
        href: "https://www.pinterest.com/pin/789/",
        querySelector: () => ({
          complete: true,
          naturalWidth: 736,
          currentSrc: "https://i.pinimg.com/736x/a.jpg",
          alt: "Mountains",
        }),
      },
    ],
  });
  assert.equal(result.pins[0].url, "https://i.pinimg.com/736x/a.jpg");
  assert.equal(result.pins[0].width, 0);
  assert.equal(result.pins[0].height, 0);
  assert.equal(result.pins[0].original_url_exact, false);
  assert.equal(result.pins[0].thumbnail_width, 736);
  assert.equal(result.pins[0].thumbnail_height, 0);
  assert.equal(result.pins[0].max_width, 0);
  assert.equal(result.pins[0].max_height, 0);
});
test("pin detail captures the observed original and rendered fallback", async () => {
  const result = await capture({
    path: "/pin/123/",
    detailTitle: "Tiny Bloom",
    json: [
      {
        entityId: "123",
        title: "Tiny Bloom",
        images_orig: {
          url: "https://i.pinimg.com/originals/12/fa/5e/12fa5e.png",
        },
      },
    ],
    detailImage: {
      src: "https://i.pinimg.com/736x/12/fa/5e/12fa5e.jpg",
      currentSrc: "https://i.pinimg.com/736x/12/fa/5e/12fa5e.jpg",
      naturalWidth: 736,
      naturalHeight: 1307,
    },
  });
  assert.equal(result.pins.length, 1);
  assert.equal(result.pins[0].id, "123");
  assert.equal(
    result.pins[0].url,
    "https://i.pinimg.com/originals/12/fa/5e/12fa5e.png",
  );
  assert.equal(
    result.pins[0].fallback_url,
    "https://i.pinimg.com/736x/12/fa/5e/12fa5e.jpg",
  );
  assert.equal(result.pins[0].width, 0);
  assert.equal(result.pins[0].height, 0);
  assert.equal(result.pins[0].original_url_exact, true);
  assert.equal(result.pins[0].thumbnail_width, 736);
  assert.equal(result.pins[0].thumbnail_height, 1307);
  assert.equal(result.pins[0].max_width, 0);
  assert.equal(result.pins[0].max_height, 0);
});
test("original metadata dimensions stay tied to the exact original URL", async () => {
  const result = await capture({
    json: [
      {
        id: "321",
        images: {
          small: {
            url: "https://i.pinimg.com/736x/321.jpg",
            width: 736,
            height: 1104,
          },
          orig: {
            url: "https://i.pinimg.com/originals/321.jpg",
            width: 2400,
            height: 3600,
          },
        },
      },
    ],
  });
  assert.equal(result.pins[0].url, "https://i.pinimg.com/originals/321.jpg");
  assert.equal(result.pins[0].thumbnail_width, 736);
  assert.equal(result.pins[0].thumbnail_height, 1104);
  assert.equal(result.pins[0].max_width, 2400);
  assert.equal(result.pins[0].max_height, 3600);
  assert.equal(
    result.pins[0].max_dimensions_url,
    "https://i.pinimg.com/originals/321.jpg",
  );
});
test("an original URL without its own metadata does not inherit thumbnail dimensions", async () => {
  const result = await capture({
    json: [
      {
        id: "654",
        images: {
          small: {
            url: "https://i.pinimg.com/736x/654.jpg",
            width: 736,
            height: 1104,
          },
          orig: { url: "https://i.pinimg.com/originals/654.jpg" },
        },
      },
    ],
  });
  assert.equal(result.pins[0].url, "https://i.pinimg.com/originals/654.jpg");
  assert.equal(result.pins[0].max_width, 0);
  assert.equal(result.pins[0].max_height, 0);
  assert.equal(result.pins[0].max_dimensions_url, undefined);
});
test("top-level images_orig metadata is imported with its matching URL", async () => {
  const result = await capture({
    json: [
      {
        id: "777",
        images_orig: {
          url: "https://i.pinimg.com/originals/777.png",
          width: 3000,
          height: 2000,
        },
        images: {
          small: {
            url: "https://i.pinimg.com/736x/777.png",
            width: 736,
            height: 491,
          },
        },
      },
    ],
  });
  assert.equal(result.pins[0].url, "https://i.pinimg.com/originals/777.png");
  assert.equal(result.pins[0].max_width, 3000);
  assert.equal(result.pins[0].max_height, 2000);
  assert.equal(
    result.pins[0].max_dimensions_url,
    "https://i.pinimg.com/originals/777.png",
  );
});
test("other origins and child frames never call native IPC", async () => {
  assert.equal(await capture({ origin: "https://evil.test" }), undefined);
  assert.equal(await capture({ child: true }), undefined);
});
test("source links stay scoped to the pin and reject Pinterest or insecure URLs", async () => {
  const result = await capture({
    json: [
      {
        id: "901",
        link: "https://pixelstalk.net/download-free-back-to-school-background/",
        images: { orig: { url: "https://i.pinimg.com/originals/901.jpg" } },
      },
      {
        id: "902",
        link: "https://www.pinterest.com/pin/902/",
        images: { orig: { url: "https://i.pinimg.com/originals/902.jpg" } },
      },
      {
        id: "903",
        link: "http://wallhaven.cc/w/903abc",
        images: { orig: { url: "https://i.pinimg.com/originals/903.jpg" } },
      },
    ],
  });
  assert.equal(result.pins.length, 3);
  assert.equal(
    result.pins.find((pin) => pin.id === "901").source_url,
    "https://pixelstalk.net/download-free-back-to-school-background/",
  );
  assert.equal(result.pins.find((pin) => pin.id === "902").source_url, undefined);
  assert.equal(result.pins.find((pin) => pin.id === "903").source_url, undefined);
});
test("import is capped and skips non-CDN media", async () => {
  const pins = Array.from({ length: 300 }, (_, i) => ({
    id: String(i + 1),
    images: {
      orig: { url: "https://i.pinimg.com/a.jpg", width: 1000, height: 500 },
    },
  }));
  pins.unshift({
    id: "999",
    images: {
      orig: { url: "https://evil.test/a.jpg", width: 1000, height: 500 },
    },
  });
  const result = await capture({ json: [pins] });
  assert.equal(result.pins.length, 200);
  assert.ok(result.pins.every((p) => p.id !== "999"));
});
test("regional Pinterest origins are accepted but lookalikes and other ports are not", async () => {
  for (const origin of [
    "https://ru.pinterest.com",
    "https://es.pinterest.com",
    "https://in.pinterest.com",
  ]) {
    const result = await capture({ origin });
    assert.equal(result.nonce, "fixture-nonce");
    assert.equal(result.page_url, origin + "/");
  }
  for (const origin of [
    "https://ru.pinterest.com.evil.test",
    "https://evilpinterest.com",
    "https://www.pinterest.com:444",
    "http://ru.pinterest.com",
  ]) {
    assert.equal(await capture({ origin }), undefined);
  }
});

// The current Pinterest page uses Relay scripts with a base64 GraphQL id and
// numeric entityId. The former JSON-only capture silently fell back to 236x.
const relayPin = {
  entityId: "615374736582874478", id: "UGluOjYxNTM3NDczNjU4Mjg3NDQ3OA==",
  images_236x: { url: "https://i.pinimg.com/236x/43/be/a3/43bea39ffbe4d4481d571e9d38ad8768.jpg", width: 236, height: 157 },
  images_orig: { url: "https://i.pinimg.com/originals/43/be/a3/43bea39ffbe4d4481d571e9d38ad8768.jpg" },
  link: "https://wallhaven.cc/w/zpvp3g",
};
test("current Relay feed imports the observed original and outbound source", async () => {
  const result = await capture({ relay: [{ data: { pin: relayPin } }] });
  assert.equal(result.pins.length, 1);
  assert.equal(result.pins[0].url, relayPin.images_orig.url);
  assert.equal(result.pins[0].original_url_exact, true);
  assert.equal(result.pins[0].source_url, relayPin.link);
  assert.equal(result.pins[0].fallback_url, relayPin.images_236x.url);
  assert.equal(result.pins[0].width, 0);
});
test("Relay detail retains original when later partial records omit images", async () => {
  const result = await capture({ path: `/pin/${relayPin.entityId}/`,
    relay: [{ data: { pin: relayPin } }, { data: { pin: { entityId: relayPin.entityId, link: relayPin.link } } }],
  });
  assert.equal(result.pins[0].url, relayPin.images_orig.url);
  assert.equal(result.pins[0].source_url, relayPin.link);
});
test("DOM capture uses observed large srcset without copying thumbnail dimensions", async () => {
  const result = await capture({ anchors: [{ href: "https://www.pinterest.com/pin/4321/",
    querySelector: () => ({ complete: true, naturalWidth: 236, naturalHeight: 157,
      currentSrc: "https://i.pinimg.com/236x/same.jpg", src: "https://i.pinimg.com/236x/same.jpg",
      srcset: "https://i.pinimg.com/236x/same.jpg 236w, https://i.pinimg.com/736x/same.jpg 736w, https://i.pinimg.com/1200x/other.jpg 1200w" }),
  }] });
  assert.equal(result.pins[0].url, "https://i.pinimg.com/736x/same.jpg");
  assert.equal(result.pins[0].fallback_url, "https://i.pinimg.com/236x/same.jpg");
  assert.equal(result.pins[0].thumbnail_width, 236);
  assert.equal(result.pins[0].width, 0);
});
test("Relay parser never evaluates injected JavaScript", async () => {
  const result = await capture({ relay: ['window.__PWS_RELAY_REGISTER_COMPLETED_REQUEST__("key",(window.executed = true));'] });
  assert.equal(result.pins.length, 0);
});
