import { useEffect, useRef, useState } from "react";
import type { AppConfig } from "../types/backup";
import { Save, Shield, Globe, ArrowUpRight, FileText, DollarSign, Route, Plus, Trash2, Key, Eye, EyeOff, X, ChevronDown } from "lucide-react";
import { useConfig } from "../hooks/useConfig";
import PageSkeleton from "../components/PageSkeleton";
import { useBeforeUnload } from "react-router-dom";
import { useLocale } from "../hooks/useLocale";

export default function SettingsPage() {
  const { config, setConfig, error, setError, save } = useConfig();
  const [saved, setSaved] = useState(false);
  const [visibleKeys, setVisibleKeys] = useState<Record<number, boolean>>({});
  const [showAdvanced, setShowAdvanced] = useState(false);
  const savedConfigRef = useRef<string>("");
  const { t } = useLocale();

  // Track the "clean" config snapshot
  useEffect(() => {
    if (config && !savedConfigRef.current) {
      savedConfigRef.current = JSON.stringify(config);
    }
  }, [config]);

  const isDirty = config ? JSON.stringify(config) !== savedConfigRef.current : false;

  // Warn on browser/tab close
  useBeforeUnload(
    (e) => {
      if (isDirty) {
        e.preventDefault();
      }
    },
    { capture: true },
  );

  useEffect(() => {
    if (!saved) return;
    const timer = setTimeout(() => setSaved(false), 5000);
    return () => clearTimeout(timer);
  }, [saved]);

  async function handleSave() {
    if (!config) return;
    setSaved(false);
    setError("");
    try {
      await save(config);
      savedConfigRef.current = JSON.stringify(config);
      setSaved(true);
    } catch (e) {
      setError(String(e));
    }
  }

  // Count how many advanced sections have non-default configuration
  const advancedCount = config
    ? [
        config.proxy.enable_logging,
        (config.proxy.daily_cost_limit ?? 0) > 0 || (config.proxy.monthly_cost_limit ?? 0) > 0,
        (config.proxy.model_aliases ?? []).length > 0,
        (config.proxy.model_routes ?? []).length > 0,
        (config.proxy.api_keys ?? []).length > 0,
      ].filter(Boolean).length
    : 0;

  if (!config) {
    return <PageSkeleton message={t("settings.loadingSettings")} />;
  }

  return (
    <div className="pb-16 space-y-6">
      <div>
        <h1 className="text-2xl font-bold">{t("settings.title")}</h1>
        <p className="text-base-content/60 mt-1">{t("settings.subtitle")}</p>
      </div>

      {error && (
        <div role="alert" className="alert alert-error">
          <span className="flex-1">{error}</span>
          <button className="btn btn-ghost btn-xs" onClick={() => setError("")}>
            <X size={14} />
          </button>
        </div>
      )}
      {saved && (
        <div role="alert" className="alert alert-success">
          <span className="flex-1">{t("settings.saved")}</span>
          <button className="btn btn-ghost btn-xs" onClick={() => setSaved(false)}>
            <X size={14} />
          </button>
        </div>
      )}

      {/* ── Core Settings ── */}
      <h2 className="text-sm font-semibold text-base-content/50 uppercase tracking-wider">
        {t("settings.coreSettings")}
      </h2>

      {/* Authentication */}
      <div className="card bg-base-100 border border-base-300">
        <div className="card-body">
          <h2 className="card-title text-sm gap-2">
            <Shield size={16} />
            {t("settings.authentication")}
          </h2>

          <div className="form-control mt-2">
            <label className="label">
              <span className="label-text">{t("settings.authMode")}</span>
            </label>
            <select
              className="select select-bordered select-sm"
              value={config.proxy.auth_mode}
              onChange={(e) =>
                setConfig({
                  ...config,
                  proxy: {
                    ...config.proxy,
                    auth_mode: e.target.value as AppConfig["proxy"]["auth_mode"],
                  },
                })
              }
            >
              <option value="auto">{t("settings.authAuto")}</option>
              <option value="off">{t("settings.authOff")}</option>
              <option value="strict">{t("settings.authStrict")}</option>
              <option value="all_except_health">{t("settings.authAllExceptHealth")}</option>
            </select>
          </div>

          <div className="form-control mt-2">
            <label className="label">
              <span className="label-text">{t("settings.adminPassword")}</span>
            </label>
            <input
              className="input input-bordered input-sm font-mono"
              type="password"
              value={config.proxy.admin_password || ""}
              placeholder={t("settings.adminPasswordPlaceholder")}
              onChange={(e) =>
                setConfig({
                  ...config,
                  proxy: {
                    ...config.proxy,
                    admin_password: e.target.value || undefined,
                  },
                })
              }
            />
          </div>
        </div>
      </div>

      {/* Network */}
      <div className="card bg-base-100 border border-base-300">
        <div className="card-body">
          <h2 className="card-title text-sm gap-2">
            <Globe size={16} />
            {t("settings.network")}
          </h2>

          <div className="form-control mt-2">
            <label className="label cursor-pointer justify-start gap-3">
              <input
                type="checkbox"
                className="toggle toggle-sm toggle-primary"
                checked={config.proxy.allow_lan_access}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    proxy: {
                      ...config.proxy,
                      allow_lan_access: e.target.checked,
                    },
                  })
                }
              />
              <span className="label-text">{t("settings.allowLan")}</span>
            </label>
          </div>

          <div className="form-control mt-2">
            <label className="label">
              <span className="label-text">{t("settings.requestTimeout")}</span>
            </label>
            <input
              className="input input-bordered input-sm"
              type="number"
              value={config.proxy.request_timeout}
              onChange={(e) =>
                setConfig({
                  ...config,
                  proxy: {
                    ...config.proxy,
                    request_timeout: Number(e.target.value),
                  },
                })
              }
            />
          </div>
        </div>
      </div>

      {/* Upstream Proxy */}
      <div className="card bg-base-100 border border-base-300">
        <div className="card-body">
          <h2 className="card-title text-sm gap-2">
            <ArrowUpRight size={16} />
            {t("settings.upstreamProxy")}
          </h2>

          <div className="form-control mt-2">
            <label className="label cursor-pointer justify-start gap-3">
              <input
                type="checkbox"
                className="toggle toggle-sm toggle-primary"
                checked={config.proxy.upstream_proxy.enabled}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    proxy: {
                      ...config.proxy,
                      upstream_proxy: {
                        ...config.proxy.upstream_proxy,
                        enabled: e.target.checked,
                      },
                    },
                  })
                }
              />
              <span className="label-text">{t("settings.enableUpstreamProxy")}</span>
            </label>
          </div>

          {config.proxy.upstream_proxy.enabled && (
            <div className="form-control mt-2">
              <label className="label">
                <span className="label-text">{t("settings.proxyUrl")}</span>
              </label>
              <input
                className="input input-bordered input-sm font-mono"
                type="text"
                placeholder="socks5://127.0.0.1:1080"
                value={config.proxy.upstream_proxy.url}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    proxy: {
                      ...config.proxy,
                      upstream_proxy: {
                        ...config.proxy.upstream_proxy,
                        url: e.target.value,
                      },
                    },
                  })
                }
              />
            </div>
          )}
        </div>
      </div>

      {/* ── Advanced Settings ── */}
      <button
        className="flex items-center gap-2 w-full group"
        onClick={() => setShowAdvanced(!showAdvanced)}
      >
        <h2 className="text-sm font-semibold text-base-content/50 uppercase tracking-wider">
          {t("settings.advancedSettings")}
        </h2>
        {advancedCount > 0 && (
          <span className="badge badge-sm badge-ghost">
            {advancedCount} {t("settings.advancedSettingsHint")}
          </span>
        )}
        <ChevronDown
          size={14}
          className={`text-base-content/40 transition-transform ${showAdvanced ? "rotate-180" : ""}`}
        />
      </button>

      {showAdvanced && (
        <div className="space-y-6">
          {/* Logging */}
          <div className="card bg-base-100 border border-base-300">
            <div className="card-body">
              <h2 className="card-title text-sm gap-2">
                <FileText size={16} />
                {t("settings.logging")}
              </h2>
              <div className="form-control mt-2">
                <label className="label cursor-pointer justify-start gap-3">
                  <input
                    type="checkbox"
                    className="toggle toggle-sm toggle-primary"
                    checked={config.proxy.enable_logging}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        proxy: {
                          ...config.proxy,
                          enable_logging: e.target.checked,
                        },
                      })
                    }
                  />
                  <span className="label-text">{t("settings.enableLogging")}</span>
                </label>
              </div>
            </div>
          </div>

          {/* Budget */}
          <div className="card bg-base-100 border border-base-300">
            <div className="card-body">
              <h2 className="card-title text-sm gap-2">
                <DollarSign size={16} />
                {t("settings.budget")}
              </h2>

              <div className="form-control mt-2">
                <label className="label">
                  <span className="label-text">{t("settings.dailyCostLimit")}</span>
                </label>
                <input
                  className="input input-bordered input-sm font-mono"
                  type="number"
                  step="0.01"
                  min="0"
                  value={config.proxy.daily_cost_limit ?? 0}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      proxy: {
                        ...config.proxy,
                        daily_cost_limit: Number(e.target.value),
                      },
                    })
                  }
                />
              </div>

              <div className="form-control mt-2">
                <label className="label">
                  <span className="label-text">{t("settings.monthlyCostLimit")}</span>
                </label>
                <input
                  className="input input-bordered input-sm font-mono"
                  type="number"
                  step="0.01"
                  min="0"
                  value={config.proxy.monthly_cost_limit ?? 0}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      proxy: {
                        ...config.proxy,
                        monthly_cost_limit: Number(e.target.value),
                      },
                    })
                  }
                />
              </div>

              <p className="text-xs text-base-content/50 mt-2">
                {t("settings.budgetWarnNote")}
              </p>
            </div>
          </div>

          {/* Model Aliases */}
          <div className="card bg-base-100 border border-base-300">
            <div className="card-body">
              <h2 className="card-title text-sm gap-2">
                <Route size={16} />
                {t("settings.modelAliases")}
              </h2>
              <p className="text-xs text-base-content/50 mb-2">
                {t("settings.modelAliasesDesc")}
              </p>
              {(config.proxy.model_aliases ?? []).map((alias, i) => (
                <div key={i} className="flex gap-2 items-center mb-1">
                  <input
                    className="input input-bordered input-sm font-mono flex-1"
                    placeholder={t("settings.patternPlaceholder")}
                    value={alias.pattern}
                    onChange={(e) => {
                      const aliases = [...(config.proxy.model_aliases ?? [])];
                      aliases[i] = { ...aliases[i], pattern: e.target.value };
                      setConfig({ ...config, proxy: { ...config.proxy, model_aliases: aliases } });
                    }}
                  />
                  <span className="text-xs text-base-content/40">&rarr;</span>
                  <input
                    className="input input-bordered input-sm font-mono flex-1"
                    placeholder={t("settings.targetModel")}
                    value={alias.target}
                    onChange={(e) => {
                      const aliases = [...(config.proxy.model_aliases ?? [])];
                      aliases[i] = { ...aliases[i], target: e.target.value };
                      setConfig({ ...config, proxy: { ...config.proxy, model_aliases: aliases } });
                    }}
                  />
                  <button
                    className="btn btn-ghost btn-xs text-error"
                    onClick={() => {
                      const aliases = (config.proxy.model_aliases ?? []).filter((_, j) => j !== i);
                      setConfig({ ...config, proxy: { ...config.proxy, model_aliases: aliases } });
                    }}
                  >
                    <Trash2 size={12} />
                  </button>
                </div>
              ))}
              <button
                className="btn btn-ghost btn-xs gap-1 mt-1 self-start"
                onClick={() => {
                  const aliases = [...(config.proxy.model_aliases ?? []), { pattern: "", target: "" }];
                  setConfig({ ...config, proxy: { ...config.proxy, model_aliases: aliases } });
                }}
              >
                <Plus size={12} /> {t("settings.addAlias")}
              </button>
            </div>
          </div>

          {/* Model Routes */}
          <div className="card bg-base-100 border border-base-300">
            <div className="card-body">
              <h2 className="card-title text-sm gap-2">
                <Route size={16} />
                {t("settings.modelRoutes")}
              </h2>
              <p className="text-xs text-base-content/50 mb-2">
                {t("settings.modelRoutesDesc")}
              </p>
              {(config.proxy.model_routes ?? []).map((route, i) => (
                <div key={i} className="flex gap-2 items-center mb-1">
                  <input
                    className="input input-bordered input-sm font-mono w-40"
                    placeholder={t("settings.modelPattern")}
                    value={route.model_pattern}
                    onChange={(e) => {
                      const routes = [...(config.proxy.model_routes ?? [])];
                      routes[i] = { ...routes[i], model_pattern: e.target.value };
                      setConfig({ ...config, proxy: { ...config.proxy, model_routes: routes } });
                    }}
                  />
                  <input
                    className="input input-bordered input-sm font-mono flex-1"
                    placeholder={t("settings.accountIds")}
                    value={route.account_ids.join(",")}
                    onChange={(e) => {
                      const routes = [...(config.proxy.model_routes ?? [])];
                      routes[i] = { ...routes[i], account_ids: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) };
                      setConfig({ ...config, proxy: { ...config.proxy, model_routes: routes } });
                    }}
                  />
                  <button
                    className="btn btn-ghost btn-xs text-error"
                    onClick={() => {
                      const routes = (config.proxy.model_routes ?? []).filter((_, j) => j !== i);
                      setConfig({ ...config, proxy: { ...config.proxy, model_routes: routes } });
                    }}
                  >
                    <Trash2 size={12} />
                  </button>
                </div>
              ))}
              <button
                className="btn btn-ghost btn-xs gap-1 mt-1 self-start"
                onClick={() => {
                  const routes = [...(config.proxy.model_routes ?? []), { model_pattern: "", account_ids: [], priority: 0 }];
                  setConfig({ ...config, proxy: { ...config.proxy, model_routes: routes } });
                }}
              >
                <Plus size={12} /> {t("settings.addRoute")}
              </button>
            </div>
          </div>

          {/* API Keys */}
          <div className="card bg-base-100 border border-base-300">
            <div className="card-body">
              <h2 className="card-title text-sm gap-2">
                <Key size={16} />
                {t("settings.apiKeys")}
              </h2>
              <p className="text-xs text-base-content/50 mb-2">
                {t("settings.apiKeysDesc")}
              </p>
              {(config.proxy.api_keys ?? []).map((apiKey, i) => (
                <div key={i} className="border border-base-300 rounded-lg p-3 mb-2 space-y-2">
                  <div className="flex gap-2 items-center">
                    <input
                      className="input input-bordered input-sm font-mono flex-1"
                      placeholder={t("settings.labelPlaceholder")}
                      value={apiKey.label}
                      onChange={(e) => {
                        const keys = [...(config.proxy.api_keys ?? [])];
                        keys[i] = { ...keys[i], label: e.target.value };
                        setConfig({ ...config, proxy: { ...config.proxy, api_keys: keys } });
                      }}
                    />
                    <div className="flex items-center gap-1 flex-1">
                      <input
                        className="input input-bordered input-sm font-mono flex-1"
                        type={visibleKeys[i] ? "text" : "password"}
                        value={apiKey.key}
                        readOnly
                      />
                      <button
                        className="btn btn-ghost btn-xs"
                        onClick={() => setVisibleKeys({ ...visibleKeys, [i]: !visibleKeys[i] })}
                      >
                        {visibleKeys[i] ? <EyeOff size={12} /> : <Eye size={12} />}
                      </button>
                    </div>
                    <label className="label cursor-pointer gap-1">
                      <input
                        type="checkbox"
                        className="toggle toggle-xs toggle-primary"
                        checked={apiKey.enabled}
                        onChange={(e) => {
                          const keys = [...(config.proxy.api_keys ?? [])];
                          keys[i] = { ...keys[i], enabled: e.target.checked };
                          setConfig({ ...config, proxy: { ...config.proxy, api_keys: keys } });
                        }}
                      />
                      <span className="label-text text-xs">{t("settings.active")}</span>
                    </label>
                    <button
                      className="btn btn-ghost btn-xs text-error"
                      onClick={() => {
                        const keys = (config.proxy.api_keys ?? []).filter((_, j) => j !== i);
                        setConfig({ ...config, proxy: { ...config.proxy, api_keys: keys } });
                      }}
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                  <div className="flex gap-2">
                    <div className="form-control flex-1">
                      <label className="label py-0"><span className="label-text text-xs">{t("settings.dailyLimit")}</span></label>
                      <input
                        className="input input-bordered input-xs font-mono"
                        type="number"
                        step="0.01"
                        min="0"
                        value={apiKey.daily_limit}
                        onChange={(e) => {
                          const keys = [...(config.proxy.api_keys ?? [])];
                          keys[i] = { ...keys[i], daily_limit: Number(e.target.value) };
                          setConfig({ ...config, proxy: { ...config.proxy, api_keys: keys } });
                        }}
                      />
                    </div>
                    <div className="form-control flex-1">
                      <label className="label py-0"><span className="label-text text-xs">{t("settings.monthlyLimit")}</span></label>
                      <input
                        className="input input-bordered input-xs font-mono"
                        type="number"
                        step="0.01"
                        min="0"
                        value={apiKey.monthly_limit}
                        onChange={(e) => {
                          const keys = [...(config.proxy.api_keys ?? [])];
                          keys[i] = { ...keys[i], monthly_limit: Number(e.target.value) };
                          setConfig({ ...config, proxy: { ...config.proxy, api_keys: keys } });
                        }}
                      />
                    </div>
                    <div className="form-control flex-1">
                      <label className="label py-0"><span className="label-text text-xs">{t("settings.allowedModels")}</span></label>
                      <input
                        className="input input-bordered input-xs font-mono"
                        placeholder="gpt-4,claude-3.5-sonnet"
                        value={apiKey.allowed_models.join(",")}
                        onChange={(e) => {
                          const keys = [...(config.proxy.api_keys ?? [])];
                          keys[i] = { ...keys[i], allowed_models: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) };
                          setConfig({ ...config, proxy: { ...config.proxy, api_keys: keys } });
                        }}
                      />
                    </div>
                  </div>
                </div>
              ))}
              <button
                className="btn btn-ghost btn-xs gap-1 mt-1 self-start"
                onClick={() => {
                  const newKey = `sk-${crypto.randomUUID().replace(/-/g, "").slice(0, 32)}`;
                  const keys = [...(config.proxy.api_keys ?? []), {
                    key: newKey,
                    label: "",
                    enabled: true,
                    daily_limit: 0,
                    monthly_limit: 0,
                    allowed_models: [],
                    created_at: Math.floor(Date.now() / 1000),
                  }];
                  setConfig({ ...config, proxy: { ...config.proxy, api_keys: keys } });
                }}
              >
                <Plus size={12} /> {t("settings.addApiKey")}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* ── Sticky Save Bar ── */}
      {isDirty && (
        <div className="fixed bottom-0 left-0 right-0 z-50 border-t border-base-300 bg-base-100/95 backdrop-blur px-6 py-3 flex items-center justify-between">
          <span className="text-sm text-warning">{t("settings.unsavedChanges")}</span>
          <button className="btn btn-primary btn-sm gap-2" onClick={handleSave}>
            <Save size={14} />
            {t("settings.saveSettings")}
          </button>
        </div>
      )}
    </div>
  );
}
