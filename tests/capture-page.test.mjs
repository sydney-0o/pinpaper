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
  anchors = [],
  origin = "https://www.pinterest.com",
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
        ? json.map((v) => ({ textContent: JSON.stringify(v) }))
        : anchors,
  };
  Object.defineProperty(document, "cookie", {
    get() {
      throw new Error("Cookie access forbidden");
    },
  });
  await vm.runInNewContext(script, {
    window,
    document,
    location: { origin, href: origin + "/" },
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
  assert.equal(result.pins[0].width, 2000);
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
});
test("other origins and child frames never call native IPC", async () => {
  assert.equal(await capture({ origin: "https://evil.test" }), undefined);
  assert.equal(await capture({ child: true }), undefined);
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
