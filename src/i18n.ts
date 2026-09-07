import en from "./locales/en.json";
import ru from "./locales/ru.json";
import es from "./locales/es.json";
import zh from "./locales/zh.json";
import hi from "./locales/hi.json";
import { resolveLanguage } from "./locale.mjs";
export { resolveLanguage };
export type Key = keyof typeof en;
const dictionaries = { en, ru, es, zh, hi };
export function translator(locale: string) {
  const dictionary = dictionaries[resolveLanguage([locale])];
  return (key: Key, values: Record<string, string | number> = {}) =>
    (dictionary[key] || en[key]).replace(/\{(\w+)\}/g, (match, name: string) =>
      String(values[name] ?? match),
    );
}
