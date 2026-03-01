const isTauri =
  typeof window !== "undefined" && "__TAURI__" in window;

type CommandMapping = Record<
  string,
  { url: string; method: "GET" | "POST" | "PUT" | "DELETE" }
>;

const COMMAND_MAPPING: CommandMapping = {
  load_config: { url: "/api/config", method: "GET" },
  save_config: { url: "/api/config", method: "POST" },
  get_proxy_status: { url: "/api/proxy/status", method: "GET" },
  proxy_start: { url: "/api/proxy/start", method: "POST" },
  proxy_stop: { url: "/api/proxy/stop", method: "POST" },
  list_accounts: { url: "/api/accounts", method: "GET" },
  get_logs: { url: "/api/logs", method: "GET" },
  replay_request: { url: "/api/logs/replay", method: "POST" },
  get_stats_summary: { url: "/api/stats/summary", method: "GET" },
  get_available_models: { url: "/api/models", method: "GET" },
  get_cli_sync_status: { url: "/api/cli/status", method: "POST" },
  execute_cli_sync: { url: "/api/cli/sync", method: "POST" },
  execute_cli_restore: { url: "/api/cli/restore", method: "POST" },
  get_cli_config_content: { url: "/api/cli/config-content", method: "POST" },
  generate_cli_config: { url: "/api/cli/generate-config", method: "POST" },
  write_cli_config: { url: "/api/cli/write-config", method: "POST" },
  validate_api_key: { url: "/api/validate-key", method: "POST" },
};

export async function request<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (isTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(cmd, args);
  }

  const mapping = COMMAND_MAPPING[cmd];
  if (!mapping) {
    throw new Error(`Unknown command: ${cmd}`);
  }

  const resp = await fetch(mapping.url, {
    method: mapping.method,
    headers: { "Content-Type": "application/json" },
    body: mapping.method !== "GET" ? JSON.stringify(args ?? {}) : undefined,
  });

  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`HTTP ${resp.status}: ${text}`);
  }

  return resp.json();
}
