import i18next from "i18next";
import { initReactI18next } from "react-i18next";
import enUS from "./locales/en-US";
import zhCN from "./locales/zh-CN";
import type { Language } from "./types";

export const LANGUAGES: readonly Language[] = ["zh-CN", "en-US"];
export const DEFAULT_LANGUAGE: Language = "zh-CN";

export function isLanguage(value: string): value is Language {
  return value === "zh-CN" || value === "en-US";
}

export const i18n = i18next;

void i18n.use(initReactI18next).init({
  resources: {
    "zh-CN": { translation: zhCN },
    "en-US": { translation: enUS },
  },
  lng: DEFAULT_LANGUAGE,
  fallbackLng: DEFAULT_LANGUAGE,
  supportedLngs: [...LANGUAGES],
  defaultNS: "translation",
  interpolation: { escapeValue: false },
  returnNull: false,
});

export async function applyLanguage(language: Language): Promise<void> {
  if (i18n.language !== language) {
    await i18n.changeLanguage(language);
  }
  if (typeof document !== "undefined") {
    document.documentElement.lang = language;
  }
}

export default i18n;
