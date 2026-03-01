import { useEffect, useState } from "react";
import { request } from "../utils/request";
import type { AppConfig } from "../types/backup";

interface UseConfigReturn {
  config: AppConfig | null;
  setConfig: React.Dispatch<React.SetStateAction<AppConfig | null>>;
  error: string;
  setError: React.Dispatch<React.SetStateAction<string>>;
  loading: boolean;
  reload: () => Promise<void>;
  save: (cfg?: AppConfig) => Promise<void>;
}

export function useConfig(): UseConfigReturn {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);

  async function load() {
    setLoading(true);
    try {
      const cfg = await request<AppConfig>("load_config");
      setConfig(cfg);
      setError("");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function save(cfg?: AppConfig) {
    const toSave = cfg ?? config;
    if (!toSave) return;
    try {
      await request("save_config", { config_data: toSave });
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    load();
  }, []);

  return { config, setConfig, error, setError, loading, reload: load, save };
}
