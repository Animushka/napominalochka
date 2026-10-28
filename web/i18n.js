// No user data is used to load translations. Language selection never authenticates requests.
const dictionaries = Object.create(null);
let language = "ru";
const emergency = {
  ru: { "brand.name": "Напоминалочка", "errors.locales": "Не удалось загрузить переводы. Обновите приложение.", "status.loading": "Собираем ваши напоминания…", "brand.telegram": "Напоминалочка — Telegram", "language.label": "Язык", "language.ru": "Русский", "language.en": "Английский", "language.ruShort": "RU", "language.enShort": "EN", "status.unavailable": "Пока не получилось", "action.retry": "Попробовать ещё раз", "action.openBot": "Открыть бота в Telegram ↗" },
  en: { "brand.name": "Napominalochka", "errors.locales": "Couldn't load translations. Please refresh the app.", "status.loading": "Gathering your reminders…", "brand.telegram": "Napominalochka — Telegram", "language.label": "Language", "language.ru": "Russian", "language.en": "English", "language.ruShort": "RU", "language.enShort": "EN", "status.unavailable": "Couldn't open that yet", "action.retry": "Try again", "action.openBot": "Open the bot in Telegram ↗" },
};

export const supportedLanguage = (value) => value === "ru" || value === "en";
export const detectLanguage = (value) => /^en(?:[-_]|$)/i.test(value || "") ? "en" : "ru";
export const getLanguage = () => language;
export const getLocale = () => language === "ru" ? "ru-RU" : "en-GB";

export function t(key, values = {}) {
  const value = dictionaries[language]?.[key] ?? emergency[language]?.[key] ?? dictionaries.en?.[key] ?? key;
  return value.replace(/\{(\w+)\}/g, (_, name) => String(values[name] ?? ""));
}

export function setLanguage(next) {
  if (!supportedLanguage(next)) return;
  language = next;
  document.documentElement.lang = next;
  document.title = t("brand.name");
  for (const element of document.querySelectorAll("[data-i18n]")) element.textContent = t(element.dataset.i18n);
  for (const element of document.querySelectorAll("[data-i18n-placeholder]")) element.setAttribute("placeholder", t(element.dataset.i18nPlaceholder));
  for (const element of document.querySelectorAll("[data-i18n-aria]")) element.setAttribute("aria-label", t(element.dataset.i18nAria));
  for (const element of document.querySelectorAll("[data-language]")) {
    element.setAttribute("aria-pressed", String(element.dataset.language === next));
    element.setAttribute("aria-label", t(`language.${element.dataset.language}`));
  }
}

export async function loadDictionaries(initialLanguage) {
  language = supportedLanguage(initialLanguage) ? initialLanguage : "ru";
  await Promise.all(["ru", "en"].map(async (name) => {
    const controller = new AbortController();
    const timeout = window.setTimeout(() => controller.abort(), 12000);
    try {
      const response = await fetch(`/locales/${name}.json`, { signal: controller.signal });
      if (!response.ok) throw new Error("locales");
      const dictionary = await response.json();
      if (!dictionary || typeof dictionary !== "object" || Object.values(dictionary).some((value) => typeof value !== "string")) throw new Error("locales");
      dictionaries[name] = dictionary;
    } finally { window.clearTimeout(timeout); }
  }));
  setLanguage(language);
}
