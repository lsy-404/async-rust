import { createI18n } from "vue-i18n";
import zhCN from "./zh-CN";
import enUS from "./en-US";
import type { Settings } from "../types";

const messages = {
  "zh-CN": zhCN,
  "en-US": enUS,
};

export type LocaleCode = keyof typeof messages;

const localeByLanguage: Record<Settings["language"], LocaleCode> = {
  zh: "zh-CN",
  en: "en-US",
};

export function localeCodeForLanguage(
  language: Settings["language"],
): LocaleCode {
  return localeByLanguage[language];
}

export function languageForLocaleCode(
  locale: LocaleCode,
): Settings["language"] {
  return locale === "en-US" ? "en" : "zh";
}

export const i18n = createI18n({
  legacy: false,
  locale: localeCodeForLanguage("zh"),
  fallbackLocale: "en-US",
  messages,
  globalInjection: true,
});

export function setLocale(language: Settings["language"]) {
  i18n.global.locale.value = localeCodeForLanguage(language);
}
