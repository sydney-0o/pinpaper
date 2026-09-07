export type Language = 'en' | 'ru' | 'es' | 'zh' | 'hi';
export const supportedLanguages: Language[];
export function resolveLanguage(locales?: readonly string[]): Language;
