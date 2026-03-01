import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { request } from "../utils/request";
import {
  Users,
  Radio,
  UserCheck,
  DollarSign,
  Zap,
  Box,
  Activity,
  TrendingUp,
  Clock,
  BarChart3,
  ArrowRight,
} from "lucide-react";
import ErrorAlert from "../components/ErrorAlert";
import PageSkeleton from "../components/PageSkeleton";
import { useConfig } from "../hooks/useConfig";
import { useLocale } from "../hooks/useLocale";

interface ProxyStatus {
  running: boolean;
}

interface AccountStats {
  total_requests: number;
  success_count: number;
  error_count: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_estimated_cost: number;
  total_duration_ms: number;
}

interface HourlyBucket {
  hour: number;
  total_requests: number;
  success_count: number;
  total_tokens: number;
  total_cost: number;
}

interface ProxyStatsData {
  per_account: Record<string, AccountStats>;
  global: AccountStats;
  per_model: Record<string, AccountStats>;
  hourly_buckets: HourlyBucket[];
}

function StatCard({
  title,
  value,
  icon: Icon,
}: {
  title: string;
  value: string | number;
  icon: React.ElementType;
}) {
  return (
    <div className="flex items-center gap-3 bg-base-100 rounded-lg border border-base-300 px-4 py-2.5">
      <div className="text-primary shrink-0">
        <Icon size={18} />
      </div>
      <div className="min-w-0">
        <div className="text-xs text-base-content/50 leading-tight">{title}</div>
        <div className="text-lg font-semibold leading-tight">{value}</div>
      </div>
    </div>
  );
}

export default function Dashboard() {
  const { config, error, setError, reload } = useConfig();
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [stats, setStats] = useState<ProxyStatsData | null>(null);
  const { t } = useLocale();

  useEffect(() => {
    loadExtraData();
  }, []);

  async function loadExtraData() {
    try {
      const [cfg, st] = await Promise.all([
        request<{ proxy_accounts: { length: number }[] }>("load_config"),
        request<ProxyStatus>("get_proxy_status").catch(() => ({ running: false })),
      ]);
      setStatus(st);

      if (cfg.proxy_accounts.length > 0) {
        setModelsLoading(true);
        request<string[]>("get_available_models")
          .then((mdls) => setModels(mdls))
          .catch(() => setModels([]))
          .finally(() => setModelsLoading(false));
      }

      request<ProxyStatsData>("get_proxy_stats")
        .then((s) => setStats(s))
        .catch(() => setStats(null));
    } catch {
      // config errors handled by useConfig
    }
  }

  if (!config) {
    return <PageSkeleton message={t("dashboard.loadingDashboard")} />;
  }

  const totalAccounts = config.accounts.length;
  const proxyAccounts = config.proxy_accounts.length;
  const enabledAccounts = config.proxy_accounts.filter((a) => !a.disabled).length;
  // New API / One API quota uses internal units: 1 USD = 500,000 units
  const QUOTA_CONVERSION_FACTOR = 500000;
  const totalQuota = config.proxy_accounts.reduce(
    (sum, a) => sum + (a.account_info?.quota ?? 0),
    0
  ) / QUOTA_CONVERSION_FACTOR;

  const successRate = stats?.global.total_requests
    ? ((stats.global.success_count / stats.global.total_requests) * 100).toFixed(1)
    : "0.0";
  const avgLatency = stats?.global.total_requests
    ? Math.round(stats.global.total_duration_ms / stats.global.total_requests)
    : 0;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">{t("dashboard.title")}</h1>
          <p className="text-base-content/60 text-sm">
            {t("dashboard.subtitle")}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span
            className={`badge badge-sm ${status?.running ? "badge-success" : "badge-error"}`}
          >
            {status?.running ? t("common.running") : t("common.stopped")}
          </span>
          {status?.running && (
            <span className="text-xs text-base-content/50">
              :{config.proxy.port}
            </span>
          )}
        </div>
      </div>

      {error && (
        <ErrorAlert
          message={error}
          onRetry={() => { reload(); loadExtraData(); }}
          onDismiss={() => setError("")}
        />
      )}

      {totalAccounts === 0 && (
        <div className="card bg-base-100 border border-base-300">
          <div className="card-body items-center text-center py-8">
            <Users size={36} className="text-base-content/20 mb-1" />
            <h2 className="card-title text-base">{t("dashboard.getStarted")}</h2>
            <p className="text-base-content/60 text-sm max-w-md">
              {t("dashboard.getStartedDesc")}
            </p>
            <div className="card-actions mt-3">
              <Link to="/accounts" className="btn btn-primary btn-sm gap-2">
                {t("dashboard.goToAccounts")}
                <ArrowRight size={14} />
              </Link>
            </div>
          </div>
        </div>
      )}

      <div className="grid grid-cols-2 lg:grid-cols-4 gap-2">
        <StatCard title={t("dashboard.totalAccounts")} value={totalAccounts} icon={Users} />
        <StatCard title={t("dashboard.proxyAccounts")} value={proxyAccounts} icon={Radio} />
        <StatCard title={t("dashboard.activeAccounts")} value={enabledAccounts} icon={UserCheck} />
        <StatCard title={t("dashboard.totalQuota")} value={`$${totalQuota.toFixed(2)}`} icon={DollarSign} />
      </div>

      {stats && stats.global.total_requests > 0 && (
        <div className="grid grid-cols-2 lg:grid-cols-4 gap-2">
          <StatCard
            title={t("dashboard.totalRequests")}
            value={stats.global.total_requests}
            icon={Activity}
          />
          <StatCard
            title={t("dashboard.estimatedCost")}
            value={`$${stats.global.total_estimated_cost.toFixed(4)}`}
            icon={DollarSign}
          />
          <StatCard
            title={t("dashboard.successRate")}
            value={`${successRate}%`}
            icon={TrendingUp}
          />
          <StatCard
            title={t("dashboard.avgLatency")}
            value={`${avgLatency}ms`}
            icon={Clock}
          />
        </div>
      )}

      {stats && Object.keys(stats.per_account).length > 0 && (
        <div className="card bg-base-100 border border-base-300">
          <div className="card-body">
            <h2 className="card-title text-sm font-medium text-base-content/60">
              <Activity size={16} />
              {t("dashboard.perAccountStats")}
            </h2>
            <div className="overflow-x-auto">
              <table className="table table-sm">
                <thead>
                  <tr>
                    <th>{t("table.account")}</th>
                    <th>{t("table.requests")}</th>
                    <th>{t("table.success")}</th>
                    <th>{t("table.errors")}</th>
                    <th>{t("table.tokens")}</th>
                    <th>{t("table.cost")}</th>
                    <th>{t("table.avgLatency")}</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(stats.per_account).map(([id, s]) => {
                    const account = config.proxy_accounts.find((a) => a.id === id);
                    const label = account
                      ? `${account.site_name} (${account.account_info.username})`
                      : id.length > 16
                        ? `${id.slice(0, 8)}...${id.slice(-4)}`
                        : id;
                    const latency = s.total_requests
                      ? Math.round(s.total_duration_ms / s.total_requests)
                      : 0;
                    return (
                      <tr key={id}>
                        <td className="text-xs max-w-[200px] truncate" title={id}>
                          {label}
                        </td>
                        <td>{s.total_requests}</td>
                        <td className="text-success">{s.success_count}</td>
                        <td className="text-error">{s.error_count}</td>
                        <td className="font-mono text-xs">
                          {s.total_input_tokens}/{s.total_output_tokens}
                        </td>
                        <td className="font-mono text-xs">
                          ${s.total_estimated_cost.toFixed(4)}
                        </td>
                        <td className="font-mono text-xs">{latency}ms</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>
        </div>
      )}

      {stats && Object.keys(stats.per_model).length > 0 && (
        <div className="card bg-base-100 border border-base-300">
          <div className="card-body">
            <h2 className="card-title text-sm font-medium text-base-content/60">
              <BarChart3 size={16} />
              {t("dashboard.topModels")}
            </h2>
            <div className="overflow-x-auto">
              <table className="table table-sm">
                <thead>
                  <tr>
                    <th>{t("table.model")}</th>
                    <th>{t("table.requests")}</th>
                    <th>{t("table.success")}</th>
                    <th>{t("table.errors")}</th>
                    <th>{t("table.tokens")}</th>
                    <th>{t("table.cost")}</th>
                    <th>{t("table.avgLatency")}</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(stats.per_model)
                    .sort(([, a], [, b]) => b.total_requests - a.total_requests)
                    .slice(0, 10)
                    .map(([model, s]) => {
                      const latency = s.total_requests
                        ? Math.round(s.total_duration_ms / s.total_requests)
                        : 0;
                      return (
                        <tr key={model}>
                          <td className="text-xs font-mono">{model}</td>
                          <td>{s.total_requests}</td>
                          <td className="text-success">{s.success_count}</td>
                          <td className="text-error">{s.error_count}</td>
                          <td className="font-mono text-xs">
                            {s.total_input_tokens}/{s.total_output_tokens}
                          </td>
                          <td className="font-mono text-xs">
                            ${s.total_estimated_cost.toFixed(4)}
                          </td>
                          <td className="font-mono text-xs">{latency}ms</td>
                        </tr>
                      );
                    })}
                </tbody>
              </table>
            </div>
          </div>
        </div>
      )}

      {stats && stats.hourly_buckets && stats.hourly_buckets.length > 0 && (
        <div className="card bg-base-100 border border-base-300">
          <div className="card-body">
            <h2 className="card-title text-sm font-medium text-base-content/60">
              <TrendingUp size={16} />
              {t("dashboard.requestTrend")}
            </h2>
            <div className="flex items-end gap-1 h-32 mt-2">
              {(() => {
                const maxReqs = Math.max(
                  ...stats.hourly_buckets.map((b) => b.total_requests),
                  1
                );
                return stats.hourly_buckets.map((bucket) => {
                  const height = Math.max(
                    (bucket.total_requests / maxReqs) * 100,
                    2
                  );
                  const hour = new Date(bucket.hour * 1000).getHours();
                  const successPct = bucket.total_requests
                    ? (bucket.success_count / bucket.total_requests) * 100
                    : 100;
                  const barColor =
                    successPct >= 90
                      ? "bg-success"
                      : successPct >= 50
                        ? "bg-warning"
                        : "bg-error";
                  return (
                    <div
                      key={bucket.hour}
                      className="flex flex-col items-center flex-1 min-w-0"
                    >
                      <div
                        className="tooltip tooltip-top w-full"
                        data-tip={`${bucket.total_requests} reqs, $${bucket.total_cost.toFixed(4)}`}
                      >
                        <div
                          className={`w-full rounded-t ${barColor}`}
                          style={{
                            height: `${height}%`,
                            minHeight: "2px",
                          }}
                        />
                      </div>
                      <span className="text-[9px] text-base-content/40 mt-1">
                        {hour}
                      </span>
                    </div>
                  );
                });
              })()}
            </div>
          </div>
        </div>
      )}

      <div className="card bg-base-100 border border-base-300">
        <div className="card-body">
          <h2 className="card-title text-sm font-medium text-base-content/60">
            <Box size={16} />
            {t("dashboard.availableModels")}
          </h2>
          {modelsLoading ? (
              <p className="text-sm text-base-content/40">
                {t("dashboard.loadingModels")}
              </p>
            ) : models.length > 0 ? (
              <div className="flex flex-wrap gap-1.5">
                {models.map((m) => (
                  <span key={m} className="badge badge-sm badge-outline">
                    {m}
                  </span>
                ))}
              </div>
            ) : (
              <p className="text-sm text-base-content/40">
                {t("dashboard.noModels")}
              </p>
            )}
        </div>
      </div>

      <div className="card bg-base-100 border border-base-300">
        <div className="card-body">
          <h2 className="card-title text-sm font-medium text-base-content/60">
            <Zap size={16} />
            {t("dashboard.quickStart")}
          </h2>
          <p className="text-sm text-base-content/60 mb-2">
            {t("dashboard.quickStartDesc")}
          </p>
          <div className="code-block">
{`# OpenAI Compatible
curl http://127.0.0.1:${config.proxy.port}/v1/chat/completions \\
  -H "Authorization: Bearer ${config.proxy.api_key}" \\
  -H "Content-Type: application/json" \\
  -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}'

# Anthropic Compatible
curl http://127.0.0.1:${config.proxy.port}/v1/messages \\
  -H "Authorization: Bearer ${config.proxy.api_key}" \\
  -H "anthropic-version: 2023-06-01" \\
  -H "Content-Type: application/json" \\
  -d '{"model":"claude-3-haiku","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}'`}
          </div>
        </div>
      </div>
    </div>
  );
}
