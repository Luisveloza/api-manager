//! Global model price cache — fetches pricing from models.dev/api.json.
//!
//! Startup: fetch once. Refresh every 24h (checked in auto_cleanup timer).
//! Network failure is completely silent — no impact on proxy functionality.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

const PRICE_API_URL: &str = "https://models.dev/api.json";
const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Cost per token for a model.
#[derive(Debug, Clone)]
pub struct ModelPrice {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
}

/// Raw response shape from models.dev/api.json.
/// Each provider has a map of model_name -> pricing info.
#[derive(Debug, Deserialize)]
struct ModelsDevEntry {
    #[serde(default)]
    pricing: Option<PricingInfo>,
}

#[derive(Debug, Deserialize)]
struct PricingInfo {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
}

pub struct PriceCache {
    prices: RwLock<HashMap<String, ModelPrice>>,
    last_refresh: RwLock<Option<Instant>>,
}

impl PriceCache {
    fn new() -> Self {
        Self {
            prices: RwLock::new(HashMap::new()),
            last_refresh: RwLock::new(None),
        }
    }

    /// Check if a refresh is needed (never fetched or >24h old).
    pub fn needs_refresh(&self) -> bool {
        let guard = self.last_refresh.read().unwrap();
        match *guard {
            None => true,
            Some(last) => last.elapsed() > REFRESH_INTERVAL,
        }
    }

    /// Refresh prices from the remote API. Silent on failure.
    pub async fn refresh(&self) {
        tracing::debug!("Refreshing model price cache from {}", PRICE_API_URL);

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "Failed to create HTTP client for price cache");
                return;
            }
        };

        let resp = match client.get(PRICE_API_URL).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::debug!(status = r.status().as_u16(), "Price API returned non-2xx");
                return;
            }
            Err(e) => {
                tracing::debug!(error = %e, "Failed to fetch price data");
                return;
            }
        };

        let body: HashMap<String, ModelsDevEntry> = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(error = %e, "Failed to parse price data JSON");
                return;
            }
        };

        let mut prices = HashMap::new();
        for (model_name, entry) in &body {
            if let Some(ref pricing) = entry.pricing {
                let input = pricing
                    .prompt
                    .as_deref()
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let output = pricing
                    .completion
                    .as_deref()
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);

                if input > 0.0 || output > 0.0 {
                    prices.insert(
                        model_name.to_lowercase(),
                        ModelPrice {
                            input_cost_per_token: input,
                            output_cost_per_token: output,
                        },
                    );
                }
            }
        }

        let count = prices.len();
        if count > 0 {
            if let Ok(mut guard) = self.prices.write() {
                *guard = prices;
            }
            if let Ok(mut guard) = self.last_refresh.write() {
                *guard = Some(Instant::now());
            }
            tracing::info!(model_count = count, "Price cache refreshed");
        }
    }

    /// Estimate cost for a request given model name and token counts.
    /// Returns None if the model is not in the price cache.
    pub fn estimate_cost(
        &self,
        model: &str,
        input_tokens: i32,
        output_tokens: i32,
    ) -> Option<f64> {
        let guard = self.prices.read().ok()?;
        let price = guard.get(&model.to_lowercase())?;
        let cost = price.input_cost_per_token * input_tokens as f64
            + price.output_cost_per_token * output_tokens as f64;
        Some(cost)
    }

    /// Check if the cache has any prices loaded.
    pub fn is_empty(&self) -> bool {
        self.prices.read().map(|g| g.is_empty()).unwrap_or(true)
    }
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static GLOBAL_PRICE_CACHE: OnceLock<Arc<PriceCache>> = OnceLock::new();

pub fn global() -> Arc<PriceCache> {
    GLOBAL_PRICE_CACHE
        .get_or_init(|| Arc::new(PriceCache::new()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_cost_empty_cache() {
        let cache = PriceCache::new();
        assert!(cache.estimate_cost("gpt-4", 100, 50).is_none());
    }

    #[test]
    fn estimate_cost_with_data() {
        let cache = PriceCache::new();
        {
            let mut guard = cache.prices.write().unwrap();
            guard.insert(
                "gpt-4".to_string(),
                ModelPrice {
                    input_cost_per_token: 0.00003,
                    output_cost_per_token: 0.00006,
                },
            );
        }
        let cost = cache.estimate_cost("gpt-4", 1000, 500).unwrap();
        let expected = 0.00003 * 1000.0 + 0.00006 * 500.0;
        assert!((cost - expected).abs() < 1e-10);
    }

    #[test]
    fn needs_refresh_initially() {
        let cache = PriceCache::new();
        assert!(cache.needs_refresh());
    }
}
