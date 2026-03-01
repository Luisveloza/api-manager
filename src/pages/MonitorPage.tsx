import { Fragment, useEffect, useState } from "react";
import { request } from "../utils/request";
import { RefreshCw, Timer, CheckCircle, XCircle, ChevronDown, ChevronRight, Copy, Play } from "lucide-react";
import { cn } from "../utils/cn";
import { useConfig } from "../hooks/useConfig";
import { useLocale } from "../hooks/useLocale";

interface ProxyRequestLog {
  id: string;
  timestamp: number;
  method: string;
  url: string;
  status: number;
  duration_ms: number;
  model?: string;
  account_id?: string;
  upstream_url?: string;
  client_ip?: string;
  input_tokens?: number;
  output_tokens?: number;
  estimated_cost?: number;
  error?: string;
  request_body?: string;
  response_body?: string;
}

interface LogsResponse {
  total: number;
  logs: ProxyRequestLog[];
}

export default function MonitorPage() {
  const [logs, setLogs] = useState<ProxyRequestLog[]>([]);
  const [total, setTotal] = useState(0);
  const [autoRefresh, setAutoRefresh] = useState(false);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [replayResult, setReplayResult] = useState<{ status: number; body: string } | null>(null);
  const [replaying, setReplaying] = useState(false);
  const { config } = useConfig();
  const [logError, setLogError] = useState("");
  const { t } = useLocale();

  useEffect(() => {
    loadLogs();
  }, []);

  useEffect(() => {
    if (!autoRefresh) return;
    const timer = setInterval(loadLogs, 3000);
    return () => clearInterval(timer);
  }, [autoRefresh]);

  async function loadLogs() {
    try {
      const data = await request<LogsResponse>("get_logs");
      setLogs(data.logs || []);
      setTotal(data.total || 0);
    } catch (e) {
      setLogError(String(e));
    }
  }

  function formatTime(ts: number): string {
    return new Date(ts * 1000).toLocaleTimeString();
  }

  function copyCurl(log: ProxyRequestLog) {
    const port = config?.proxy.port ?? 3000;
    const parts = [`curl -X ${log.method} 'http://localhost:${port}${log.url}'`];
    parts.push("-H 'Content-Type: application/json'");
    if (log.request_body) {
      const escaped = log.request_body.replace(/'/g, "'\\''");
      parts.push(`-d '${escaped}'`);
    }
    navigator.clipboard.writeText(parts.join(" \\\n  "));
  }

  async function handleReplay(logId: string) {
    setReplaying(true);
    setReplayResult(null);
    try {
      const result = await request<{ status: number; body: string }>("replay_request", { log_id: logId });
      setReplayResult(result);
    } catch (e) {
      setReplayResult({ status: 0, body: String(e) });
    } finally {
      setReplaying(false);
    }
  }

  function formatJson(raw: string): string {
    try {
      return JSON.stringify(JSON.parse(raw), null, 2);
    } catch {
      return raw;
    }
  }

  const successCount = logs.filter((l) => l.status >= 200 && l.status < 300).length;
  const errorCount = logs.filter((l) => l.status >= 400).length;
  const totalCost = logs.reduce((sum, l) => sum + (l.estimated_cost ?? 0), 0);

  return (
    <div className="space-y-6">
      <div className="flex justify-between items-center">
        <div>
          <h1 className="text-2xl font-bold">{t("monitor.title")}</h1>
          <p className="text-base-content/60 mt-1">
            {t("monitor.subtitle")} ({total} {t("common.total").toLowerCase()})
          </p>
        </div>
        <div className="flex gap-2">
          <button className="btn btn-ghost btn-sm gap-2" onClick={loadLogs}>
            <RefreshCw size={14} />
            {t("common.refresh")}
          </button>
          <button
            className={cn(
              "btn btn-sm gap-2",
              autoRefresh ? "btn-primary" : "btn-ghost"
            )}
            onClick={() => setAutoRefresh(!autoRefresh)}
          >
            <Timer size={14} />
            {autoRefresh ? t("monitor.autoOn") : t("monitor.autoOff")}
          </button>
        </div>
      </div>

      {logError && (
        <div role="alert" className="alert alert-error">
          <span>{logError}</span>
        </div>
      )}

      <div className="stats stats-horizontal bg-base-100 border border-base-300 w-full">
        <div className="stat">
          <div className="stat-title">{t("common.total")}</div>
          <div className="stat-value text-lg">{total}</div>
        </div>
        <div className="stat">
          <div className="stat-figure text-success">
            <CheckCircle size={20} />
          </div>
          <div className="stat-title">{t("common.success")}</div>
          <div className="stat-value text-lg text-success">{successCount}</div>
        </div>
        <div className="stat">
          <div className="stat-figure text-error">
            <XCircle size={20} />
          </div>
          <div className="stat-title">{t("common.error")}</div>
          <div className="stat-value text-lg text-error">{errorCount}</div>
        </div>
        {totalCost > 0 && (
          <div className="stat">
            <div className="stat-title">{t("monitor.estCost")}</div>
            <div className="stat-value text-lg">${totalCost.toFixed(4)}</div>
          </div>
        )}
      </div>

      {logs.length === 0 ? (
        <div className="card bg-base-100 border border-base-300">
          <div className="card-body items-center text-center py-12">
            <p className="text-base-content/60">{t("monitor.noLogs")}</p>
            <p className="text-sm text-base-content/40 mt-1">
              {t("monitor.noLogsHint")}
            </p>
          </div>
        </div>
      ) : (
        <div className="card bg-base-100 border border-base-300">
          <div className="overflow-x-auto">
            <table className="table table-sm">
              <thead>
                <tr>
                  <th></th>
                  <th>{t("monitor.time")}</th>
                  <th>{t("monitor.method")}</th>
                  <th>{t("monitor.path")}</th>
                  <th>{t("monitor.upstream")}</th>
                  <th>{t("monitor.model")}</th>
                  <th>{t("monitor.status")}</th>
                  <th>{t("monitor.duration")}</th>
                  <th>{t("monitor.tokens")}</th>
                  <th>{t("monitor.cost")}</th>
                  <th>{t("monitor.actions")}</th>
                </tr>
              </thead>
              <tbody>
                {logs.map((log) => (
                  <Fragment key={log.id}>
                    <tr className="hover cursor-pointer" onClick={() => setExpandedId(expandedId === log.id ? null : log.id)}>
                      <td className="w-6">
                        {expandedId === log.id ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                      </td>
                      <td className="font-mono text-xs">
                        {formatTime(log.timestamp)}
                      </td>
                      <td>
                        <span className="badge badge-outline badge-xs">
                          {log.method}
                        </span>
                      </td>
                      <td className="text-xs max-w-[200px] truncate">{log.url}</td>
                      <td className="text-xs max-w-[200px] truncate" title={log.upstream_url}>{log.upstream_url || "-"}</td>
                      <td className="text-xs">{log.model || "-"}</td>
                      <td>
                        <span
                          className={cn(
                            "badge badge-sm",
                            log.status >= 200 && log.status < 300
                              ? "badge-success"
                              : log.status >= 400 && log.status < 500
                                ? "badge-warning"
                                : "badge-error"
                          )}
                        >
                          {log.status}
                        </span>
                      </td>
                      <td className="text-xs font-mono">{log.duration_ms}ms</td>
                      <td className="text-xs">
                        {log.input_tokens != null
                          ? `${log.input_tokens}/${log.output_tokens}`
                          : "-"}
                      </td>
                      <td className="text-xs font-mono">
                        {log.estimated_cost != null
                          ? `$${log.estimated_cost.toFixed(4)}`
                          : "-"}
                      </td>
                      <td onClick={(e) => e.stopPropagation()}>
                        <div className="flex gap-1">
                          <button
                            className="btn btn-ghost btn-xs tooltip"
                            data-tip={t("monitor.copyAsCurl")}
                            onClick={() => copyCurl(log)}
                          >
                            <Copy size={12} />
                          </button>
                          {log.request_body && (
                            <button
                              className="btn btn-ghost btn-xs tooltip"
                              data-tip={t("monitor.replay")}
                              disabled={replaying}
                              onClick={() => handleReplay(log.id)}
                            >
                              <Play size={12} />
                            </button>
                          )}
                        </div>
                      </td>
                    </tr>
                    {expandedId === log.id && (
                      <tr key={`${log.id}-detail`}>
                        <td colSpan={11} className="bg-base-200/50 p-0">
                          <div className="p-4 space-y-3">
                            {log.error && (
                              <div className="text-error text-xs">{t("common.error")}: {log.error}</div>
                            )}
                            {log.client_ip && (
                              <div className="text-xs text-base-content/60">{t("monitor.clientIp")}: {log.client_ip}</div>
                            )}
                            <div className="grid grid-cols-1 lg:grid-cols-2 gap-3">
                              <div>
                                <div className="text-xs font-semibold mb-1">{t("monitor.requestBody")}</div>
                                <pre className="bg-base-300 rounded p-2 text-xs overflow-auto max-h-64 whitespace-pre-wrap break-all">
                                  {log.request_body ? formatJson(log.request_body) : t("monitor.notCaptured")}
                                </pre>
                              </div>
                              <div>
                                <div className="text-xs font-semibold mb-1">{t("monitor.responseBody")}</div>
                                <pre className="bg-base-300 rounded p-2 text-xs overflow-auto max-h-64 whitespace-pre-wrap break-all">
                                  {log.response_body ? formatJson(log.response_body) : t("monitor.notCaptured")}
                                </pre>
                              </div>
                            </div>
                            {replayResult && expandedId === log.id && (
                              <div>
                                <div className="text-xs font-semibold mb-1">{t("monitor.replayResult")} (status: {replayResult.status})</div>
                                <pre className="bg-base-300 rounded p-2 text-xs overflow-auto max-h-48 whitespace-pre-wrap break-all">
                                  {formatJson(replayResult.body)}
                                </pre>
                              </div>
                            )}
                          </div>
                        </td>
                      </tr>
                    )}
                  </Fragment>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
