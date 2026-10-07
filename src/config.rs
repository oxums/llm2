use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------- top-level ----------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub aliases: HashMap<String, AliasConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Proxy + admin API key. Empty means "take from API_KEY env or default admin".
    #[serde(default)]
    pub api_key: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { host: default_host(), port: default_port(), api_key: String::new() }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    3000
}

// ---------- providers ----------

/// Provider ids (lowercase): openai, anthropic, google, google-vertex,
/// azure-openai, mistral, cohere, xai, deepseek, groq, togetherai, fireworks,
/// deepinfra, cerebras, perplexity, amazon-bedrock, elevenlabs, openrouter,
/// ollama, lmstudio, vercel, typesafe, openai-compatible
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// null = all fetched models; otherwise allowlist of upstream model ids
    #[serde(default)]
    pub enabled_models: Option<Vec<String>>,
    /// azure only
    #[serde(default)]
    pub api_version: Option<String>,
}

impl ProviderConfig {
    pub fn is_linked(&self) -> bool {
        !self.api_key.trim().is_empty()
    }
}

// ---------- aliases ----------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AliasConfig {
    #[serde(default)]
    pub models: HashMap<String, AliasModelConfig>,
    #[serde(default = "default_pick_mode")]
    pub pick_mode: String,
    #[serde(default)]
    pub order: Vec<String>,
    /// image|audio|video|pdf -> auto|on|off
    #[serde(default)]
    pub capabilities_spoof: HashMap<String, String>,
    #[serde(default)]
    pub classifier: ClassifierConfig,
}

impl Default for AliasConfig {
    fn default() -> Self {
        Self {
            models: HashMap::new(),
            pick_mode: default_pick_mode(),
            order: vec![],
            capabilities_spoof: HashMap::new(),
            classifier: ClassifierConfig::default(),
        }
    }
}

fn default_pick_mode() -> String {
    "failover".to_string()
}

/// Key format: "provider/model" (upstream id may itself contain slashes).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AliasModelConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    #[serde(default)]
    pub extra_body: serde_json::Value,
    /// modality -> force value. absent = inherit from catalog.
    #[serde(default)]
    pub capabilities_override: HashMap<String, bool>,
    #[serde(default)]
    pub option_description: Option<String>,
    #[serde(default)]
    pub embed_dimensions: Option<u32>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClassifierConfig {
    /// jev | text_structured
    #[serde(default = "default_clf_backend")]
    pub backend: String,
    #[serde(default = "default_clf_model")]
    pub model: String,
    #[serde(default)]
    pub fallback: String,
    #[serde(default = "default_clf_prompt")]
    pub prompt: String,
    #[serde(default = "default_min_conf")]
    pub min_confidence: f64,
    #[serde(default = "default_clf_timeout")]
    pub timeout_ms: u64,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            backend: default_clf_backend(),
            model: default_clf_model(),
            fallback: String::new(),
            prompt: default_clf_prompt(),
            min_confidence: default_min_conf(),
            timeout_ms: default_clf_timeout(),
        }
    }
}

fn default_clf_backend() -> String {
    "jev".to_string()
}
fn default_clf_model() -> String {
    "jev-latest".to_string()
}
fn default_clf_prompt() -> String {
    "Which model should handle this request?".to_string()
}
fn default_min_conf() -> f64 {
    0.5
}
fn default_clf_timeout() -> u64 {
    15000
}

// ---------- loading ----------

pub fn default_config_path() -> String {
    std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.json".to_string())
}

pub fn load_config(path: &str) -> AppConfig {
    let mut cfg: AppConfig = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // env overlay for server
    if let Ok(h) = std::env::var("HOST") {
        if !h.is_empty() {
            cfg.server.host = h;
        }
    }
    if let Ok(p) = std::env::var("PORT") {
        if let Ok(n) = p.parse::<u16>() {
            cfg.server.port = n;
        }
    }
    if let Ok(k) = std::env::var("API_KEY") {
        if !k.is_empty() {
            cfg.server.api_key = k;
        }
    }
    if cfg.server.api_key.is_empty() {
        cfg.server.api_key = "admin".to_string();
    }
    cfg
}

pub fn save_config(path: &str, cfg: &AppConfig) -> Result<(), String> {
    let s = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| e.to_string())
}
