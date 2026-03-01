import { createContext, useContext, useState, useCallback, type ReactNode } from "react";
import en, { type TranslationKey } from "../locales/en";
import zh from "../locales/zh";

export type Locale = "en" | "zh";

const translations = { en, zh } as const;

function detectLocale(): Locale {
  try {
    const stored = localStorage.getItem("locale");
    if (stored === "en" || stored === "zh") return stored;
  } catch {
    // localStorage unavailable
  }
  const lang = navigator.language || "";
  return lang.startsWith("zh") ? "zh" : "en";
}

interface LocaleContextValue {
  locale: Locale;
  setLocale: (locale: Locale) => void;
  t: (key: TranslationKey) => string;
}

const LocaleContext = createContext<LocaleContextValue | null>(null);

export function LocaleProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(detectLocale);

  const setLocale = useCallback((next: Locale) => {
    setLocaleState(next);
    try {
      localStorage.setItem("locale", next);
    } catch {
      // ignore
    }
  }, []);

  const t = useCallback(
    (key: TranslationKey): string => translations[locale][key],
    [locale],
  );

  return (
    <LocaleContext.Provider value={{ locale, setLocale, t }}>
      {children}
    </LocaleContext.Provider>
  );
}

export function useLocale(): LocaleContextValue {
  const ctx = useContext(LocaleContext);
  if (!ctx) throw new Error("useLocale must be used within LocaleProvider");
  return ctx;
}
