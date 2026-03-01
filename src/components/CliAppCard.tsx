import {
  CheckCircle2,
  AlertCircle,
  RefreshCw,
  Loader2,
  Eye,
  RotateCcw,
  FlaskConical,
} from "lucide-react";
import { cn } from "../utils/cn";
import { useLocale } from "../hooks/useLocale";

interface CliStatus {
  installed: boolean;
  version: string | null;
  is_synced: boolean;
  has_backup: boolean;
  current_base_url: string | null;
  files: string[];
}

interface CliProbeResult {
  cli_name: string;
  config_found: boolean;
  config_valid: boolean;
  proxy_reachable: boolean;
  auth_ok: boolean;
  model_available: boolean;
  response_valid: boolean;
  error: string | null;
  latency_ms: number;
}

interface CliAppCardProps {
  app: string;
  name: string;
  icon: React.ReactNode;
  status: CliStatus | null;
  isLoading: boolean;
  isSyncing: boolean;
  isTesting: boolean;
  probeResult: CliProbeResult | null;
  onViewConfig: () => void;
  onRestore: () => void;
  onTest: () => void;
  onSync: () => void;
}

export default function CliAppCard({
  name,
  icon,
  status,
  isLoading,
  isSyncing,
  isTesting,
  probeResult,
  onViewConfig,
  onRestore,
  onTest,
  onSync,
}: CliAppCardProps) {
  const { t } = useLocale();

  return (
    <div className="flex flex-col bg-base-100 rounded-xl border border-base-300 p-4 hover:shadow-md transition-all">
      {/* Header */}
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-3">
          <div className="p-2 bg-base-200 rounded-lg">{icon}</div>
          <div>
            <h4 className="text-sm font-bold">{name}</h4>
            <div className="mt-0.5">
              {isLoading ? (
                <span className="text-[10px] text-base-content/40 flex items-center gap-1">
                  <Loader2 size={10} className="animate-spin" />
                  {t("cli.detecting")}
                </span>
              ) : status?.installed ? (
                <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-info/10 text-info font-bold">
                  v{status.version || "?"}
                </span>
              ) : (
                <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-base-200 text-base-content/40">
                  {t("cli.notInstalled")}
                </span>
              )}
            </div>
          </div>
        </div>

        {!isLoading && status?.installed && (
          <div
            className={cn(
              "flex items-center gap-1 px-2 py-1 rounded-full text-[10px] font-bold",
              status.is_synced
                ? "bg-success/10 text-success"
                : "bg-warning/10 text-warning",
            )}
          >
            {status.is_synced ? (
              <>
                <CheckCircle2 size={12} /> {t("cli.synced")}
              </>
            ) : (
              <>
                <AlertCircle size={12} /> {t("cli.notSynced")}
              </>
            )}
          </div>
        )}
      </div>

      {/* Base URL */}
      <div className="p-2 bg-base-200/50 rounded-lg border border-dashed border-base-300 mb-3">
        <div className="text-[9px] text-base-content/40 uppercase font-bold tracking-wider mb-1">
          {t("cli.currentBaseUrl")}
        </div>
        <div className="text-[10px] font-mono truncate text-base-content/50 italic">
          {status?.current_base_url || "---"}
        </div>
      </div>

      {/* Actions */}
      <div className="mt-auto flex items-center gap-2">
        {status?.installed && (
          <>
            <button
              onClick={onViewConfig}
              className="btn btn-ghost btn-xs"
              title={t("cli.viewConfig")}
            >
              <Eye size={14} />
            </button>
            {status.has_backup && (
              <button
                onClick={onRestore}
                className="btn btn-ghost btn-xs"
                title={t("cli.restore")}
                disabled={isSyncing}
              >
                <RotateCcw size={14} />
              </button>
            )}
          </>
        )}
        <button
          onClick={onTest}
          disabled={isTesting || !status?.installed || isLoading}
          className="btn btn-ghost btn-xs"
          title={t("cli.testConnection")}
        >
          {isTesting ? (
            <Loader2 size={14} className="animate-spin" />
          ) : (
            <FlaskConical size={14} />
          )}
        </button>
        <button
          onClick={onSync}
          disabled={!status?.installed || isSyncing || isLoading}
          className={cn(
            "btn btn-sm flex-1 gap-2",
            status?.is_synced ? "btn-ghost" : "btn-primary",
          )}
        >
          {isSyncing ? (
            <Loader2 size={14} className="animate-spin" />
          ) : (
            <RefreshCw size={14} />
          )}
          {t("common.sync")}
        </button>
      </div>

      {/* Probe Results */}
      {probeResult && (
        <div className="mt-3 p-2 bg-base-200/50 rounded-lg border border-dashed border-base-300">
          <div className="text-[9px] text-base-content/40 uppercase font-bold tracking-wider mb-1.5">
            {t("cli.testResults")} {probeResult.latency_ms > 0 && `(${probeResult.latency_ms}ms)`}
          </div>
          <div className="grid grid-cols-2 gap-1 text-[10px]">
            {([
              [t("cli.configFound"), probeResult.config_found],
              [t("cli.configValid"), probeResult.config_valid],
              [t("cli.proxyReachable"), probeResult.proxy_reachable],
              [t("cli.authOk"), probeResult.auth_ok],
              [t("cli.modelAvailable"), probeResult.model_available],
              [t("cli.responseValid"), probeResult.response_valid],
            ] as const).map(([label, ok]) => (
              <div key={label} className="flex items-center gap-1">
                {ok ? (
                  <CheckCircle2 size={10} className="text-success" />
                ) : (
                  <AlertCircle size={10} className="text-error" />
                )}
                <span className={ok ? "text-success" : "text-error"}>
                  {label}
                </span>
              </div>
            ))}
          </div>
          {probeResult.error && (
            <div className="mt-1 text-[10px] text-error truncate" title={probeResult.error}>
              {probeResult.error}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
