export const supportedLanguages = ["en", "ru", "es", "zh", "hi"];
export function resolveLanguage(locales = []) {
  const primary = (locales[0] || "").toLowerCase().split(/[-_.:]/)[0];
  return supportedLanguages.includes(primary) ? primary : "en";
}
