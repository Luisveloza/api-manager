import { useState, useEffect, useCallback } from "react";
import { request } from "../utils/request";
import {
  Terminal,
  Cpu,
  Globe,
  CodeXml,
  Bot,
} from "lucide-react";
import CliAppCard from "./CliAppCard";
import ConfigEditorModal from "./ConfigEditorModal";
import { useLocale } from "../hooks/useLocale";

type CliAppType = "Claude" | "Codex" | "Gemini" | "OpenCode" | "Droid";

interface CliStatus {
  installed: boolean;
  version: string | null;
  is_synced: boolean;
  has_backup: boolean;
  current_base_url: string | null;
  files: string[];
}

interface ClaudeModelConfig {
  model: string | null;
  primaryModel: string | null;
  haikuModel: string | null;
  opusModel: string | null;
  sonnetModel: string | null;
  reasoningModel: string | null;
}

interface CliSyncCardProps {
  proxyUrl: string;
  apiKey: string;
  proxyPort: number;
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

interface EditorState {
  app: CliAppType;
  fileName: string;
  allFiles: string[];
  content: string;
  isGenerated: boolean;
  isValid: boolean;
}

const CLI_APPS: CliAppType[] = ["Claude", "Codex", "Gemini", "OpenCode", "Droid"];

const iconMap: Record<CliAppType, React.ReactNode> = {
  Claude: <CodeXml size={20} className="text-purple-500" />,
  Codex: <Cpu size={20} className="text-blue-500" />,
  Gemini: <Globe size={20} className="text-green-500" />,
  OpenCode: <CodeXml size={20} className="text-cyan-500" />,
  Droid: <Bot size={20} className="text-orange-500" />,
};

const nameMap: Record<CliAppType, string> = {
  Claude: "Claude Code",
  Codex: "Codex CLI",
  Gemini: "Gemini CLI",
  OpenCode: "OpenCode",
  Droid: "Droid",
};

function buildClaudeModelConfig(models: string[]): ClaudeModelConfig {
  const claude = models.filter(
    (m) => m.includes("claude") || m.includes("anthropic"),
  );
  if (claude.length === 0) {
    return { model: null, primaryModel: null, haikuModel: null, opusModel: null, sonnetModel: null, reasoningModel: null };
  }

  const findBest = (keywords: string[], fallback?: string): string | null => {
    for (const kw of keywords) {
      const match = claude.find((m) => m.includes(kw));
      if (match) return match;
    }
    return fallback || null;
  };

  const opus = findBest(["opus-4-6", "opus-4", "opus"]);
  const sonnet = findBest(["sonnet-4", "claude-4-sonnet", "sonnet-3-5", "sonnet-3.5", "sonnet"]);
  const haiku = findBest(["haiku-4-5", "haiku-4", "haiku-3.5", "haiku"]);
  const primary = opus || sonnet || haiku || claude[0];
  const reasoning = opus || primary;
  const modelAlias = opus ? "opus" : sonnet ? "sonnet" : "haiku";

  return {
    model: modelAlias,
    primaryModel: primary,
    haikuModel: haiku || primary,
    opusModel: opus || primary,
    sonnetModel: sonnet || primary,
    reasoningModel: reasoning,
  };
}

function isValidJson(str: string): boolean {
  try { JSON.parse(str); return true; } catch { return false; }
}

function initRecord<T>(val: T): Record<CliAppType, T> {
  return { Claude: val, Codex: val, Gemini: val, OpenCode: val, Droid: val };
}

export default function CliSyncCard({ proxyUrl, apiKey, proxyPort }: CliSyncCardProps) {
  const [statuses, setStatuses] = useState<Record<CliAppType, CliStatus | null>>(initRecord(null));
  const [loading, setLoading] = useState<Record<CliAppType, boolean>>(initRecord(false));
  const [syncing, setSyncing] = useState<Record<CliAppType, boolean>>(initRecord(false));
  const [testing, setTesting] = useState<Record<CliAppType, boolean>>(initRecord(false));
  const [probeResults, setProbeResults] = useState<Record<CliAppType, CliProbeResult | null>>(initRecord(null));
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [editorState, setEditorState] = useState<EditorState | null>(null);
  const { t } = useLocale();

  const getFormattedProxyUrl = useCallback(
    (app: CliAppType) => {
      if (!proxyUrl) return "";
      const base = proxyUrl.trimEnd().replace(/\/+$/, "");
      if (app === "Codex" || app === "OpenCode") {
        return base.endsWith("/v1") ? base : `${base}/v1`;
      }
      return base.replace(/\/v1$/, "");
    },
    [proxyUrl],
  );

  const checkStatus = useCallback(
    async (app: CliAppType) => {
      setLoading((prev) => ({ ...prev, [app]: true }));
      try {
        const status = await request<CliStatus>("get_cli_sync_status", {
          appType: app,
          proxyUrl: getFormattedProxyUrl(app),
        });
        setStatuses((prev) => ({ ...prev, [app]: status }));
      } catch (err) {
        console.error(`Failed to check ${app} status:`, err);
      } finally {
        setLoading((prev) => ({ ...prev, [app]: false }));
      }
    },
    [getFormattedProxyUrl],
  );

  useEffect(() => {
    request<string[]>("get_available_models").then(setAvailableModels).catch(() => {});
  }, []);

  useEffect(() => {
    CLI_APPS.forEach(checkStatus);
  }, [checkStatus]);

  const getClaudeModels = (): ClaudeModelConfig | null => {
    if (availableModels.length === 0) return null;
    return buildClaudeModelConfig(availableModels);
  };

  const handleSync = async (app: CliAppType) => {
    if (!proxyUrl || !apiKey) return;
    const status = statuses[app];
    if (!status) return;
    const firstFile = status.files[0];
    try {
      const content = await request<string>("generate_cli_config", {
        appType: app, proxyUrl: getFormattedProxyUrl(app), apiKey,
        model: null, claudeModels: app === "Claude" ? getClaudeModels() : null, fileName: firstFile,
      });
      const isJson = firstFile.endsWith(".json");
      setEditorState({
        app, fileName: firstFile, allFiles: status.files, content,
        isGenerated: true, isValid: isJson ? isValidJson(content) : true,
      });
    } catch (err) {
      console.error("Failed to generate config:", err);
    }
  };

  const handleSwitchFile = async (fileName: string) => {
    if (!editorState) return;
    const { app } = editorState;
    try {
      const content = await request<string>("generate_cli_config", {
        appType: app, proxyUrl: getFormattedProxyUrl(app), apiKey,
        model: null, claudeModels: app === "Claude" ? getClaudeModels() : null, fileName,
      });
      const isJson = fileName.endsWith(".json");
      setEditorState({ ...editorState, fileName, content, isGenerated: true, isValid: isJson ? isValidJson(content) : true });
    } catch (err) {
      console.error("Failed to generate config:", err);
    }
  };

  const handleApply = async () => {
    if (!editorState || !editorState.isValid) return;
    const { app, fileName, content } = editorState;
    setSyncing((prev) => ({ ...prev, [app]: true }));
    try {
      await request("write_cli_config", { appType: app, fileName, content });
      for (const f of editorState.allFiles) {
        if (f === fileName) continue;
        try {
          const autoContent = await request<string>("generate_cli_config", {
            appType: app, proxyUrl: getFormattedProxyUrl(app), apiKey,
            model: null, claudeModels: app === "Claude" ? getClaudeModels() : null, fileName: f,
          });
          await request("write_cli_config", { appType: app, fileName: f, content: autoContent });
        } catch { /* non-critical */ }
      }
      setEditorState(null);
      await checkStatus(app);
    } catch (err) {
      console.error("Failed to write config:", err);
    } finally {
      setSyncing((prev) => ({ ...prev, [app]: false }));
    }
  };

  const handleRestore = async (app: CliAppType) => {
    setSyncing((prev) => ({ ...prev, [app]: true }));
    try {
      await request("execute_cli_restore", { appType: app });
      await checkStatus(app);
    } catch (err) {
      console.error("Restore failed:", err);
    } finally {
      setSyncing((prev) => ({ ...prev, [app]: false }));
    }
  };

  const handleTest = async (app: CliAppType) => {
    setTesting((prev) => ({ ...prev, [app]: true }));
    setProbeResults((prev) => ({ ...prev, [app]: null }));
    try {
      const result = await request<CliProbeResult>("probe_cli_compatibility", { appType: app, proxyPort, apiKey });
      setProbeResults((prev) => ({ ...prev, [app]: result }));
    } catch (err) {
      console.error(`Probe ${app} failed:`, err);
      setProbeResults((prev) => ({
        ...prev,
        [app]: {
          cli_name: app, config_found: false, config_valid: false,
          proxy_reachable: false, auth_ok: false, model_available: false,
          response_valid: false, error: String(err), latency_ms: 0,
        },
      }));
    } finally {
      setTesting((prev) => ({ ...prev, [app]: false }));
    }
  };

  const handleViewConfig = async (app: CliAppType, fileName?: string) => {
    const status = statuses[app];
    if (!status) return;
    const targetFile = fileName || status.files[0];
    try {
      const content = await request<string>("get_cli_config_content", { appType: app, fileName: targetFile });
      setEditorState({ app, fileName: targetFile, allFiles: status.files, content, isGenerated: false, isValid: true });
    } catch (err) {
      console.error("Failed to read config:", err);
    }
  };

  const handleViewSwitchFile = async (fileName: string) => {
    if (!editorState) return;
    try {
      const content = await request<string>("get_cli_config_content", { appType: editorState.app, fileName });
      setEditorState({ ...editorState, fileName, content, isGenerated: false, isValid: true });
    } catch (err) {
      console.error("Failed to read config:", err);
    }
  };

  return (
    <div className="space-y-4">
      <div className="px-1 flex items-center justify-between">
        <div className="flex items-center gap-2 text-base-content/60">
          <Terminal size={14} />
          <span className="text-xs font-bold uppercase tracking-widest">{t("cli.configSync")}</span>
        </div>
        <p className="text-xs text-base-content/40 italic">{t("cli.configSyncDesc")}</p>
      </div>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
        {CLI_APPS.map((app) => (
          <CliAppCard
            key={app}
            app={app}
            name={nameMap[app]}
            icon={iconMap[app]}
            status={statuses[app]}
            isLoading={loading[app]}
            isSyncing={syncing[app]}
            isTesting={testing[app]}
            probeResult={probeResults[app]}
            onViewConfig={() => handleViewConfig(app)}
            onRestore={() => handleRestore(app)}
            onTest={() => handleTest(app)}
            onSync={() => handleSync(app)}
          />
        ))}
      </div>

      {editorState && (
        <ConfigEditorModal
          editorState={editorState}
          appIcon={iconMap[editorState.app]}
          appName={nameMap[editorState.app]}
          syncing={syncing[editorState.app]}
          onClose={() => setEditorState(null)}
          onChange={(val) => {
            const isJson = editorState.fileName.endsWith(".json");
            setEditorState({ ...editorState, content: val, isValid: isJson ? isValidJson(val) : true });
          }}
          onSwitchFile={(f) => editorState.isGenerated ? handleSwitchFile(f) : handleViewSwitchFile(f)}
          onApply={handleApply}
        />
      )}
    </div>
  );
}
