import { I18nProvider as AriaI18nProvider } from "react-aria-components";
import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import type { Lang } from "@/shared/api/contracts";
import { getTelegram } from "@/shared/lib/telegram";
import ru from "./messages/ru.json";
import en from "./messages/en.json";

export type TranslationValues = Record<string, string | number>;
export type Translate = (key: string, values?: TranslationValues) => string;

const messages: Record<Lang, Record<string, string>> = { ru, en };
export const detectLanguage = (value?: string): Lang => /^en(?:[-_]|$)/i.test(value || "") ? "en" : "ru";
export const supportedLanguage = (value: unknown): value is Lang => value === "ru" || value === "en";

interface I18nContextValue {
  t: Translate;
  language: Lang;
  setLanguage: (language: Lang) => void;
  locale: "ru-RU" | "en-GB";
}

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({ children, initialLanguage }: { children: ReactNode; initialLanguage?: Lang }) {
  const [language, setLanguage] = useState<Lang>(() => initialLanguage ?? detectLanguage(getTelegram()?.initDataUnsafe?.user?.language_code));
  const value = useMemo<I18nContextValue>(() => ({
    language,
    setLanguage,
    locale: language === "ru" ? "ru-RU" : "en-GB",
    t: (key, values = {}) => (messages[language][key] ?? messages.en[key] ?? key)
      .replace(/\{(\w+)\}/g, (_, name: string) => String(values[name] ?? "")),
  }), [language]);

  useEffect(() => {
    document.documentElement.lang = language;
    document.title = messages[language]["brand.name"];
  }, [language]);

  return <I18nContext.Provider value={value}><AriaI18nProvider locale={value.locale}>{children}</AriaI18nProvider></I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  const value = useContext(I18nContext);
  if (!value) throw new Error("I18nProvider is required");
  return value;
}
