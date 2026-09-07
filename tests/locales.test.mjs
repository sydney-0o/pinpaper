import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolveLanguage, supportedLanguages } from "../src/locale.mjs";
const read = (lang) =>
  JSON.parse(
    readFileSync(
      new URL(`../src/locales/${lang}.json`, import.meta.url),
      "utf8",
    ),
  );
test("five complete locales preserve every message placeholder", () => {
  const en = read("en");
  const placeholders = (s) =>
    [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
  assert.equal(supportedLanguages.length, 5);
  for (const lang of supportedLanguages) {
    const dictionary = read(lang);
    assert.deepEqual(Object.keys(dictionary).sort(), Object.keys(en).sort());
    for (const key of Object.keys(en)) {
      assert.ok(dictionary[key].trim(), `${lang}.${key} is empty`);
      assert.deepEqual(
        placeholders(dictionary[key]),
        placeholders(en[key]),
        `${lang}.${key} placeholders`,
      );
    }
  }
});
test("regional system languages resolve with English fallback", () => {
  for (const [locale, expected] of [
    ["ru-RU", "ru"],
    ["es-MX", "es"],
    ["zh-Hant-TW", "zh"],
    ["hi-IN", "hi"],
    ["en-GB", "en"],
    ["fr-FR", "en"],
    ["", "en"],
  ])
    assert.equal(resolveLanguage([locale]), expected);
  assert.equal(resolveLanguage([]), "en");
});
