export interface AccountInfo {
  id: number;
  access_token: string;
  api_key?: string;
  username: string;
  quota: number;
  today_prompt_tokens: number;
  today_completion_tokens: number;
  today_quota_consumption: number;
  today_requests_count: number;
  today_income: number;
}

export interface HealthStatus {
  status: string;
  reason?: string;
}

export interface SiteAccount {
  id: string;
  site_name: string;
  site_url: string;
  site_type: string;
  authType: "access_token" | "cookie" | "none";
  account_info: AccountInfo;
  health?: HealthStatus;
  disabled?: boolean;
  exchange_rate?: number;
  notes?: string;
  last_sync_time?: number;
  updated_at?: number;
  created_at?: number;
  proxy_priority?: number;
  proxy_weight?: number;
}

export interface ProxyConfig {
  enabled: boolean;
  port: number;
  api_key: string;
  admin_password?: string;
  auth_mode: "off" | "strict" | "all_except_health" | "auto";
  load_balance_mode?: "round_robin" | "failover" | "random" | "weighted";
  daily_cost_limit?: number;
  monthly_cost_limit?: number;
  budget_exceeded_action?: "warn" | "block";
  model_aliases?: Array<{ pattern: string; target: string }>;
  model_routes?: Array<{
    model_pattern: string;
    account_ids: string[];
    priority: number;
  }>;
  api_keys?: Array<{
    key: string;
    label: string;
    enabled: boolean;
    daily_limit: number;
    monthly_limit: number;
    allowed_models: string[];
    created_at: number;
  }>;
  allow_lan_access: boolean;
  auto_start: boolean;
  request_timeout: number;
  enable_logging: boolean;
  upstream_proxy: {
    enabled: boolean;
    url: string;
  };
}

export interface AppConfig {
  proxy: ProxyConfig;
  accounts: SiteAccount[];
  proxy_accounts: SiteAccount[];
}

export interface BackupV2 {
  version: string;
  timestamp: number;
  type: string;
  accounts: {
    accounts: RawAccountEntry[];
    bookmarks?: unknown[];
    pinnedAccountIds?: string[];
    orderedAccountIds?: string[];
    last_updated?: number;
  };
  tagStore?: unknown;
}

export interface RawAccountEntry {
  id?: string;
  site_name: string;
  site_url: string;
  site_type: string;
  authType: string;
  account_info: AccountInfo;
  health?: HealthStatus;
  disabled?: boolean;
  exchange_rate?: number;
  notes?: string;
  last_sync_time?: number;
  updated_at?: number;
  created_at?: number;
  [key: string]: unknown;
}
