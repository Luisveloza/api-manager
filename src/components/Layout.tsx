import { Outlet, NavLink } from "react-router-dom";
import {
  LayoutDashboard,
  Users,
  Radio,
  Activity,
  Settings,
} from "lucide-react";
import { cn } from "../utils/cn";
import ThemeToggle from "./ThemeToggle";
import LocaleToggle from "./LocaleToggle";
import { useLocale } from "../hooks/useLocale";
import type { TranslationKey } from "../locales/en";

const navItems: { to: string; labelKey: TranslationKey; icon: typeof LayoutDashboard }[] = [
  { to: "/", labelKey: "nav.dashboard", icon: LayoutDashboard },
  { to: "/accounts", labelKey: "nav.accounts", icon: Users },
  { to: "/proxy", labelKey: "nav.proxy", icon: Radio },
  { to: "/monitor", labelKey: "nav.monitor", icon: Activity },
  { to: "/settings", labelKey: "nav.settings", icon: Settings },
];

export default function Layout() {
  const { t } = useLocale();

  return (
    <div className="min-h-screen bg-base-200">
      <div className="navbar bg-base-100/80 backdrop-blur-md border-b border-base-300 px-6 sticky top-0 z-30">
        <div className="flex-1">
          <span className="text-lg font-bold">APIManager</span>
        </div>
        <div className="flex-none flex items-center gap-2">
          <ul className="menu menu-horizontal gap-1">
            {navItems.map((item) => {
              const Icon = item.icon;
              const label = t(item.labelKey);
              return (
                <li key={item.to}>
                  <NavLink
                    to={item.to}
                    end={item.to === "/"}
                    title={label}
                    className={({ isActive }) =>
                      cn(
                        "gap-2 text-sm",
                        isActive && "active font-medium"
                      )
                    }
                  >
                    <Icon size={16} />
                    <span className="hidden sm:inline">{label}</span>
                  </NavLink>
                </li>
              );
            })}
          </ul>
          <LocaleToggle />
          <ThemeToggle />
        </div>
      </div>
      <main className="p-6 max-w-6xl mx-auto">
        <Outlet />
      </main>
    </div>
  );
}
