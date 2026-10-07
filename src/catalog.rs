use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Normalized capabilities for one "provider/model" entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCaps {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub image: bool,
    pub audio: bool,
    pub video: bool,
    pub pdf: bool,
    pub tool_call: bool,
    pub structured: bool,
    pub reasoning: bool,
    pub embedding: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionModel {
    /// "typesafe" | "openrouter"
    pub source: String,
    /// upstream slug, e.g. "typesafe/jev-1.13" or "cloudflare/clef"
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// key "provider/modelid(lowercased provider + original id)"
    pub models: HashMap<String, ModelCaps>,
    pub decisions: Vec<DecisionModel>,
    pub fetched_at: Option<String>,
    pub providers_seen: Vec<String>,
}

impl Catalog {
    pub fn key(provider: &str, id: &str) -> String {
        format!("{}/{}", provider.to_lowercase(), id)
    }
    pub fn get(&self, provider: &str, id: &str) -> Option<&ModelCaps> {
        self.models.get(&Self::key(provider, id))
    }
    /// all providers serving this upstream model id (chat pool)
    pub fn providers_for(&self, model_id: &str) -> Vec<String> {
        let mut out = vec![];
        for c in self.models.values() {
            if !c.embedding && c.id == model_id {
                out.push(c.provider.clone());
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// Decision slugs we support through OpenRouter (models.dev omits these).
pub const OPENROUTER_DECISION_ALLOW: &[&str] = &[
    "cloudflare/clef",
    "cloudflare/clef-flash",
    "liquid/d1",
    "respan/span-01",
    "respan/span-01-lite",
    "jaredpalmer/kev-4b",
    "openai/gpt-6-luna-decisions",
    "perplexity/pplx-decider-v1-27b",
];

fn bool_in(v: &serde_json::Value, key: &str) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}

fn modalities_of(m: &serde_json::Value) -> Vec<String> {
    let mut out = vec![];
    if let Some(mo) = m.get("modalities") {
        if let Some(inp) = mo.get("input").and_then(|x| x.as_array()) {
            for x in inp {
                if let Some(s) = x.as_str() {
                    out.push(s.to_lowercase());
                }
            }
        }
    }
    // some dumps use attachment:true as image-ish hint; keep strict to modalities.
    out
}

/// Parse https://models.dev/api.json defensively. Shape:
/// { "<provider>": { id?, name?, models: { "<modelid>": {...} } } }
pub fn parse_models_dev(root: &serde_json::Value) -> Catalog {
    let mut cat = Catalog::default();
    let obj = match root.as_object() {
        Some(o) => o,
        None => return cat,
    };
    for (prov, pval) in obj {
        if prov.starts_with('_') || prov.starts_with('$') {
            continue;
        }
        let models = match pval.get("models").and_then(|m| m.as_object()) {
            Some(m) => m,
            None => continue,
        };
        // skip non-provider top-level keys that lack models map shape
        let provider = prov.to_lowercase();
        if !cat.providers_seen.contains(&provider) {
            cat.providers_seen.push(provider.clone());
        }
        for (mid, mval) in models {
            let mo = modalities_of(mval);
            let has = |s: &str| mo.iter().any(|x| x == s);
            let name = mval.get("name").and_then(|x| x.as_str()).unwrap_or(mid).to_string();
            let lowered = format!("{} {}", mid.to_lowercase(), name.to_lowercase());
            let embedding = lowered.contains("embed");
            let caps = ModelCaps {
                provider: provider.clone(),
                id: mid.clone(),
                name,
                image: has("image"),
                audio: has("audio"),
                video: has("video"),
                pdf: has("pdf"),
                tool_call: bool_in(mval, "tool_call"),
                structured: bool_in(mval, "structured_output"),
                reasoning: bool_in(mval, "reasoning"),
                embedding,
            };
            cat.models.insert(Catalog::key(&provider, mid), caps);
        }
    }
    cat.providers_seen.sort();
    cat
}

pub async fn fetch_models_dev(http: &reqwest::Client) -> Option<serde_json::Value> {
    let url = std::env::var("MODELS_DEV_URL").unwrap_or_else(|_| "https://models.dev/api.json".to_string());
    let resp = http.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<serde_json::Value>().await.ok()
}

/// OpenRouter decisions catalog. Public endpoint; caller gates on openrouter linked.
pub async fn fetch_openrouter_decisions(http: &reqwest::Client) -> Vec<DecisionModel> {
    let mut out = vec![];
    let resp = match http.get("https://openrouter.ai/api/v1/models?output_modalities=decisions").send().await {
        Ok(r) => r,
        Err(_) => return out,
    };
    if !resp.status().is_success() {
        return out;
    }
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return out,
    };
    let arr = match v.get("data").and_then(|d| d.as_array()) {
        Some(a) => a,
        None => return out,
    };
    for m in arr {
        let id = m.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let low = id.to_lowercase();
        let keep = low.starts_with("typesafe/") || OPENROUTER_DECISION_ALLOW.iter().any(|a| *a == low);
        if !keep {
            continue;
        }
        out.push(DecisionModel {
            source: "openrouter".to_string(),
            slug: id.clone(),
            name: m.get("name").and_then(|x| x.as_str()).unwrap_or(&id).to_string(),
        });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

pub fn builtin_typesafe_decisions() -> Vec<DecisionModel> {
    vec![
        DecisionModel { source: "typesafe".into(), slug: "typesafe/jev-latest".into(), name: "Jev latest".into() },
        DecisionModel { source: "typesafe".into(), slug: "typesafe/jev-preview".into(), name: "Jev preview".into() },
        DecisionModel { source: "typesafe".into(), slug: "typesafe/jev-1.13.0".into(), name: "Jev 1.13".into() },
    ]
}

pub async fn refresh_catalog(
    http: &reqwest::Client,
    openrouter_linked: bool,
    typesafe_linked: bool,
) -> Catalog {
    let mut cat = fetch_models_dev(http).await.map(|v| parse_models_dev(&v)).unwrap_or_default();
    let mut decisions = vec![];
    if typesafe_linked {
        decisions.extend(builtin_typesafe_decisions());
    }
    if openrouter_linked {
        decisions.extend(fetch_openrouter_decisions(http).await);
    }
    cat.decisions = decisions;
    cat.fetched_at = Some(chrono::Utc::now().to_rfc3339());
    cat
}
