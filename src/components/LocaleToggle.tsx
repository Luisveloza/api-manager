import { Languages } from "lucide-react";
import { useLocale } from "../hooks/useLocale";

export default function LocaleToggle() {
  const { locale, setLocale } = useLocale();

  return (
    <button
      className="btn btn-ghost btn-sm btn-square"
      onClick={() => setLocale(locale === "en" ? "zh" : "en")}
      aria-label={locale === "en" ? "切换到中文" : "Switch to English"}
      title={locale === "en" ? "中文" : "English"}
    >
      <Languages size={16} />
    </button>
  );
}
