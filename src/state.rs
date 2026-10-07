use crate::catalog::Catalog;
use crate::config::AppConfig;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub type Shared = Arc<Inner>;

pub struct Inner {
    pub config: tokio::sync::RwLock<AppConfig>,
    pub config_path: String,
    pub api_key: tokio::sync::RwLock<String>,
    pub http: reqwest::Client,
    pub catalog: tokio::sync::RwLock<Catalog>,
    /// model key -> cooldown expiry
    pub cooldowns: DashMap<String, Instant>,
    /// alias name -> round robin counter
    pub rr: DashMap<String, AtomicU64>,
    pub metrics: Metrics,
}

pub struct Metrics {
    pub requests: AtomicU64,
    pub errors: AtomicU64,
    pub chat: AtomicU64,
    pub embeddings: AtomicU64,
    pub decisions: AtomicU64,
    pub tokens_in: AtomicU64,
    pub tokens_out: AtomicU64,
    pub lat_sum_ms: AtomicU64,
    pub lat_count: AtomicU64,
    pub start: Instant,
}

impl Metrics {
    pub fn snapshot(&self) -> serde_json::Value {
        let count = self.lat_count.load(Ordering::Relaxed);
        let avg = if count > 0 { self.lat_sum_ms.load(Ordering::Relaxed) / count } else { 0 };
        serde_json::json!({
            "requests": self.requests.load(Ordering::Relaxed),
            "errors": self.errors.load(Ordering::Relaxed),
            "chat": self.chat.load(Ordering::Relaxed),
            "embeddings": self.embeddings.load(Ordering::Relaxed),
            "decisions": self.decisions.load(Ordering::Relaxed),
            "tokens_in": self.tokens_in.load(Ordering::Relaxed),
            "tokens_out": self.tokens_out.load(Ordering::Relaxed),
            "avg_latency_ms": avg,
            "uptime_s": self.start.elapsed().as_secs(),
        })
    }
}

pub fn new_shared(cfg: AppConfig, config_path: String) -> Shared {
    let api_key = cfg.server.api_key.clone();
    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(256)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("http client");
    Arc::new(Inner {
        config: tokio::sync::RwLock::new(cfg),
        config_path,
        api_key: tokio::sync::RwLock::new(api_key),
        http,
        catalog: tokio::sync::RwLock::new(Catalog::default()),
        cooldowns: DashMap::new(),
        rr: DashMap::new(),
        metrics: Metrics {
            requests: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            chat: AtomicU64::new(0),
            embeddings: AtomicU64::new(0),
            decisions: AtomicU64::new(0),
            tokens_in: AtomicU64::new(0),
            tokens_out: AtomicU64::new(0),
            lat_sum_ms: AtomicU64::new(0),
            lat_count: AtomicU64::new(0),
            start: Instant::now(),
        },
    })
}

pub fn err(status: axum::http::StatusCode, code: &str, msg: impl Into<String>) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    (
        status,
        axum::Json(serde_json::json!({"error": {"code": code, "message": msg.into(), "type": "proxy_error"}})),
    )
}

/// Split "provider:model" or "provider/model" into (provider, upstream_id).
/// Provider is lowercased; upstream id keeps original case.
pub fn split_ref(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(i) = s.find(':') {
        let (p, m) = s.split_at(i);
        let m = &m[1..];
        if p.is_empty() || m.is_empty() {
            return None;
        }
        return Some((p.to_lowercase(), m.to_string()));
    }
    if let Some(i) = s.find('/') {
        let (p, m) = s.split_at(i);
        let m = &m[1..];
        if p.is_empty() || m.is_empty() {
            return None;
        }
        // guard: bare model names without provider look like "gpt-4o" (no slash).
        // "a/b/c" -> provider "a", upstream "b/c".
        return Some((p.to_lowercase(), m.to_string()));
    }
    None
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
