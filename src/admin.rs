use crate::catalog::Catalog;
use crate::config::{save_config, AliasConfig, ProviderConfig};
use crate::proxy::authorized;
use crate::state::{err, Shared};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::Value;

fn need_auth<'a>(st: &Shared, h: &'a HeaderMap) -> bool {
    // sync wrapper not possible (async); callers check inline
    let _ = (st, h);
    true
}

// GET /api/me
pub async fn me(State(st): State<Shared>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    Json(serde_json::json!({"ok": true})).into_response()
}

// GET /api/stats
pub async fn stats(State(st): State<Shared>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    let cfg = st.config.read().await;
    let cat = st.catalog.read().await;
    Json(serde_json::json!({
        "metrics": st.metrics.snapshot(),
        "providers": cfg.providers.len(),
        "aliases": cfg.aliases.len(),
        "models": cat.models.len(),
        "decision_models": cat.decisions.len(),
        "fetched_at": cat.fetched_at,
    }))
    .into_response()
}

// GET /api/providers
pub async fn list_providers(State(st): State<Shared>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    let cfg = st.config.read().await;
    let cat = st.catalog.read().await;
    let mut out = vec![];
    for id in provider_ids() {
        let p = cfg.providers.get(id);
        let linked = p.map(|x| x.is_linked()).unwrap_or(false);
        let base = p.and_then(|x| x.base_url.clone()).unwrap_or_default();
        let eff = if base.is_empty() {
            crate::providers::default_base(id).unwrap_or("").to_string()
        } else {
            base
        };
        let n = cat.models.values().filter(|m| m.provider == *id).count();
        out.push(serde_json::json!({
            "id": id,
            "linked": linked,
            "has_key": linked,
            "base_url": p.and_then(|x| x.base_url.clone()),
            "effective_base_url": eff,
            "enabled_models": p.and_then(|x| x.enabled_models.clone()),
            "extra_headers": p.map(|x| x.extra_headers.clone()).unwrap_or_default(),
            "api_version": p.and_then(|x| x.api_version.clone()),
            "models_count": n,
            "kind": provider_kind(id),
        }));
    }
    // custom providers present in config but not in fixed list
    for (id, p) in &cfg.providers {
        if !provider_ids().contains(&id.as_str()) {
            out.push(serde_json::json!({
                "id": id, "linked": p.is_linked(), "has_key": p.is_linked(),
                "base_url": p.base_url, "effective_base_url": p.base_url.clone().unwrap_or_default(),
                "enabled_models": p.enabled_models, "extra_headers": p.extra_headers,
                "api_version": p.api_version, "models_count": 0, "kind": "custom",
            }));
        }
    }
    let _ = need_auth;
    Json(out).into_response()
}

pub fn provider_ids() -> Vec<&'static str> {
    vec![
        "openai", "anthropic", "google", "google-vertex", "azure-openai", "mistral", "cohere",
        "xai", "deepseek", "groq", "togetherai", "fireworks", "deepinfra", "cerebras",
        "perplexity", "amazon-bedrock", "elevenlabs", "openrouter", "ollama", "lmstudio",
        "vercel", "typesafe", "openai-compatible",
    ]
}

fn provider_kind(id: &str) -> &'static str {
    match id {
        "typesafe" => "decision",
        "amazon-bedrock" | "google-vertex" | "elevenlabs" => "unsupported",
        _ => "chat",
    }
}

// PUT /api/providers/:id
pub async fn put_provider(State(st): State<Shared>, headers: HeaderMap, Path(id): Path<String>, Json(patch): Json<Value>) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    let id = id.to_lowercase();
    let snapshot = {
        let mut guard = st.config.write().await;
        let mut next = (**guard).clone();
        {
            let e = next.providers.entry(id.clone()).or_insert_with(ProviderConfig::default);
            if let Some(k) = patch.get("api_key").and_then(|v| v.as_str()) {
                e.api_key = k.to_string();
            }
            if let Some(b) = patch.get("base_url") {
                e.base_url = b.as_str().map(|s| s.to_string());
            }
            if let Some(h) = patch.get("extra_headers").and_then(|v| v.as_object()) {
                e.extra_headers = h.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect();
            }
            if let Some(em) = patch.get("enabled_models") {
                e.enabled_models = if em.is_null() {
                    None
                } else {
                    em.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                };
            }
            if let Some(v) = patch.get("api_version").and_then(|x| x.as_str()) {
                e.api_version = Some(v.to_string());
            }
        }
        let arc = std::sync::Arc::new(next);
        *guard = arc.clone();
        arc
    };
    let path = st.config_path.clone();
    if let Err(e) = save_config(&path, &snapshot) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "save", e).into_response();
    }
    // refresh catalog in background (link state may have changed)
    let st2 = st.clone();
    tokio::spawn(async move { refresh_now(&st2).await });
    tracing::info!(provider = %id, "provider updated");
    Json(serde_json::json!({"ok": true, "id": id})).into_response()
}

// GET /api/aliases
pub async fn list_aliases(State(st): State<Shared>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    let cfg = st.config.read().await;
    let cat = st.catalog.read().await;
    let mut out = serde_json::Map::new();
    for (name, a) in &cfg.aliases {
        let mut v = serde_json::to_value(a).unwrap_or(Value::Null);
        v["kind"] = Value::String(crate::router::alias_kind_full(name, &cfg, &cat, &|k| decision_member(&cat, k)).to_string());
        out.insert(name.clone(), v);
    }
    Json(Value::Object(out)).into_response()
}

pub fn decision_member(cat: &Catalog, member_key: &str) -> bool {
    if let Some(rest) = member_key.strip_prefix("openrouter/") {
        return cat.decisions.iter().any(|d| d.source == "openrouter" && d.slug == rest) || is_known_decision_slug(rest);
    }
    if member_key.starts_with("typesafe/") {
        return true;
    }
    cat.decisions.iter().any(|d| d.slug == member_key)
}

fn is_known_decision_slug(slug: &str) -> bool {
    crate::catalog::OPENROUTER_DECISION_ALLOW.iter().any(|a| *a == slug.to_lowercase())
}

// PUT /api/aliases/:name
pub async fn put_alias(State(st): State<Shared>, headers: HeaderMap, Path(name): Path<String>, Json(a): Json<AliasConfig>) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    if !["round_robin", "failover", "classifier", "custom_order"].contains(&a.pick_mode.as_str()) {
        return err(StatusCode::BAD_REQUEST, "bad_pick_mode", "pick_mode must be round_robin|failover|classifier|custom_order").into_response();
    }
    if !["jev", "text_structured"].contains(&a.classifier.backend.as_str()) {
        return err(StatusCode::BAD_REQUEST, "bad_backend", "classifier.backend must be jev|text_structured").into_response();
    }
    // kind uniformity check
    {
        let cfg = st.config.read().await;
        let cat = st.catalog.read().await;
        let mut kinds = std::collections::HashSet::new();
        for k in a.models.keys() {
            if decision_member(&cat, k) {
                kinds.insert("decision");
            } else if let Some((p, id)) = crate::state::split_ref(k) {
                let emb = cat.get(&p, &id).map(|c| c.embedding).unwrap_or_else(|| id.to_lowercase().contains("embed"));
                kinds.insert(if emb { "embedding" } else { "chat" });
            }
        }
        if kinds.len() > 1 {
            return err(StatusCode::BAD_REQUEST, "mixed_kind", "alias members must be all chat, all embedding, or all decision").into_response();
        }
        let _ = cfg;
    }
    let snapshot = {
        let mut guard = st.config.write().await;
        let mut next = (**guard).clone();
        tracing::info!(alias = %name, mode = %a.pick_mode, members = a.models.len(), "alias saving");
        next.aliases.insert(name.clone(), a);
        let arc = std::sync::Arc::new(next);
        *guard = arc.clone();
        arc
    };
    let path = st.config_path.clone();
    if let Err(e) = save_config(&path, &snapshot) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "save", e).into_response();
    }
    tracing::info!(alias = %name, "alias saved");
    Json(serde_json::json!({"ok": true, "id": name})).into_response()
}

// DELETE /api/aliases/:name
pub async fn del_alias(State(st): State<Shared>, headers: HeaderMap, Path(name): Path<String>) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    let snapshot = {
        let mut guard = st.config.write().await;
        let mut next = (**guard).clone();
        next.aliases.remove(&name);
        let arc = std::sync::Arc::new(next);
        *guard = arc.clone();
        arc
    };
    let path = st.config_path.clone();
    if let Err(e) = save_config(&path, &snapshot) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "save", e).into_response();
    }
    Json(serde_json::json!({"ok": true})).into_response()
}

// GET /api/catalog
pub async fn catalog_view(State(st): State<Shared>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    let cat = st.catalog.read().await;
    let cfg = st.config.read().await;
    // group: provider -> [{id, name, caps}]
    let mut groups: std::collections::HashMap<String, Vec<Value>> = Default::default();
    for c in cat.models.values() {
        groups.entry(c.provider.clone()).or_default().push(serde_json::json!({
            "id": c.id, "name": c.name, "image": c.image, "audio": c.audio,
            "video": c.video, "pdf": c.pdf, "tool_call": c.tool_call,
            "structured": c.structured, "reasoning": c.reasoning, "embedding": c.embedding,
        }));
    }
    for v in groups.values_mut() {
        v.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    }
    let linked: Vec<String> = cfg.providers.iter().filter(|(_, p)| p.is_linked()).map(|(k, _)| k.clone()).collect();
    Json(serde_json::json!({
        "providers": groups,
        "providers_seen": cat.providers_seen,
        "linked": linked,
        "decisions": cat.decisions,
        "fetched_at": cat.fetched_at,
    }))
    .into_response()
}

// POST /api/reload
pub async fn reload(State(st): State<Shared>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "invalid key").into_response();
    }
    refresh_now(&st).await;
    let cat = st.catalog.read().await;
    Json(serde_json::json!({"ok": true, "models": cat.models.len(), "decisions": cat.decisions.len(), "fetched_at": cat.fetched_at})).into_response()
}

pub async fn refresh_now(st: &Shared) {
    let (or_linked, ts_linked) = {
        let cfg = st.config.read().await;
        (
            cfg.providers.get("openrouter").map(|p| p.is_linked()).unwrap_or(false),
            cfg.providers.get("typesafe").map(|p| p.is_linked()).unwrap_or(false),
        )
    };
    let cat = crate::catalog::refresh_catalog(&st.http, or_linked, ts_linked).await;
    *st.catalog.write().await = std::sync::Arc::new(cat);
}
