use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method, Proxy};
use std::time::Duration;

use crate::models::UpstreamProxyConfig;

/// Headers that should NOT be forwarded to upstream.
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "host",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "authorization",
];

/// HTTP client for forwarding requests to upstream API providers.
pub struct UpstreamClient {
    client: Client,
    timeout: Duration,
}

impl UpstreamClient {
    pub fn new(timeout: Duration, upstream_proxy: Option<&UpstreamProxyConfig>) -> Self {
        let mut builder = Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10));

        if let Some(proxy_config) = upstream_proxy {
            if proxy_config.enabled && !proxy_config.url.is_empty() {
                if let Ok(proxy) = Proxy::all(&proxy_config.url) {
                    builder = builder.proxy(proxy);
                    tracing::info!(url = %proxy_config.url, "Upstream proxy configured");
                }
            }
        }

        Self {
            client: builder.build().unwrap_or_else(|_| Client::new()),
            timeout,
        }
    }

    /// Forward a request to the upstream site.
    ///
    /// URL is built as: `{site_url}{path}` — caller provides the full path
    /// (e.g. `/v1/chat/completions`).
    pub async fn forward(
        &self,
        site_url: &str,
        path: &str,
        method: Method,
        headers: HeaderMap,
        body: Bytes,
        access_token: &str,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let url = format!("{}{}", site_url.trim_end_matches('/'), path);

        let mut forwarded_headers = filter_hop_headers(&headers);
        forwarded_headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", access_token)).unwrap_or_else(|_| {
                HeaderValue::from_static("Bearer invalid")
            }),
        );

        self.client
            .request(method, &url)
            .headers(forwarded_headers)
            .body(body)
            .timeout(self.timeout)
            .send()
            .await
    }

    /// Forward with a custom header name for the auth token (e.g. `x-api-key` for Anthropic).
    pub async fn forward_with_custom_auth(
        &self,
        site_url: &str,
        path: &str,
        method: Method,
        headers: HeaderMap,
        body: Bytes,
        access_token: &str,
        auth_header: &str,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let url = format!("{}{}", site_url.trim_end_matches('/'), path);

        let mut forwarded_headers = filter_hop_headers(&headers);

        // Set both Bearer and custom auth header
        forwarded_headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", access_token)).unwrap_or_else(|_| {
                HeaderValue::from_static("Bearer invalid")
            }),
        );
        if let Ok(name) = HeaderName::from_bytes(auth_header.as_bytes()) {
            if let Ok(value) = HeaderValue::from_str(access_token) {
                forwarded_headers.insert(name, value);
            }
        }

        // Ensure anthropic-version is present for New API routing
        // (New API requires both x-api-key AND anthropic-version to route to Claude adaptor)
        if auth_header == "x-api-key" {
            if !forwarded_headers.contains_key("anthropic-version") {
                forwarded_headers.insert(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_static("2023-06-01"),
                );
            }
        }

        self.client
            .request(method, &url)
            .headers(forwarded_headers)
            .body(body)
            .timeout(self.timeout)
            .send()
            .await
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

fn filter_hop_headers(headers: &HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    for (name, value) in headers.iter() {
        let name_lower = name.as_str().to_lowercase();
        if !HOP_BY_HOP_HEADERS.contains(&name_lower.as_str()) {
            filtered.insert(name.clone(), value.clone());
        }
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header;

    #[test]
    fn upstream_builds_correct_url() {
        // Verify URL construction logic (without making real HTTP requests)
        let site_url = "https://api.example.com/";
        let path = "/v1/chat/completions";
        let result = format!("{}{}", site_url.trim_end_matches('/'), path);
        assert_eq!(result, "https://api.example.com/v1/chat/completions");

        // No trailing slash
        let site_url2 = "https://api.example.com";
        let result2 = format!("{}{}", site_url2.trim_end_matches('/'), path);
        assert_eq!(result2, "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn upstream_strips_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost"));
        headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer old-token"),
        );
        headers.insert(
            HeaderName::from_static("x-custom"),
            HeaderValue::from_static("keep-me"),
        );

        let filtered = filter_hop_headers(&headers);

        assert!(!filtered.contains_key(header::HOST));
        assert!(!filtered.contains_key(header::CONNECTION));
        assert!(!filtered.contains_key(header::AUTHORIZATION));
        assert!(filtered.contains_key(header::CONTENT_TYPE));
        assert!(filtered.contains_key("x-custom"));
    }
}
