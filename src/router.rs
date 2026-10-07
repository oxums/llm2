use crate::catalog::{Catalog, ModelCaps};
use crate::config::{AliasConfig, AppConfig};
use crate::state::{split_ref, Shared};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- flags

#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub ignore: Vec<String>,
    pub prefer: Vec<String>,
    pub any_prefer_provider: Option<String>,
    pub only_providers: Option<Vec<String>>,
    pub only_models: Option<Vec<String>>,
    pub prefer_ignore_cooldown: bool,
}

fn parse_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
        Some(Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        _ => vec![],
    }
}

fn header_list(h: &axum::http::HeaderMap, name: &str) -> Option<Vec<String>> {
    h.get(name).and_then(|v| v.to_str().ok()).map(|s| {
        s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
    }).filter(|v: &Vec<String>| !v.is_empty())
}

pub fn extract_flags(body: &mut Value, headers: &axum::http::HeaderMap) -> Flags {
    let mut f = Flags { prefer_ignore_cooldown: true, ..Default::default() };
    f.ignore = parse_list(body.get("ignore_models"));
    f.prefer = parse_list(body.get("prefer_models"));
    f.any_prefer_provider = body.get("any_prefer_provider").and_then(|v| v.as_str()).map(|s| s.to_lowercase());
    f.only_providers = body.get("only_providers").map(|v| {
        parse_list(Some(v)).into_iter().map(|s| s.to_lowercase()).collect()
    });
    f.only_models = body.get("only_models").map(parse_list_owned);
    f.prefer_ignore_cooldown = body.get("prefer_models_ignore_cooldown").and_then(|v| v.as_bool()).unwrap_or(true);

    // headers win when present
    if let Some(v) = header_list(headers, "x-ignore-models") {
        f.ignore = v;
    }
    if let Some(v) = header_list(headers, "x-prefer-models") {
        f.prefer = v;
    }
    if let Some(v) = headers.get("x-any-prefer-provider").and_then(|x| x.to_str().ok()) {
        let s = v.trim().to_lowercase();
        if !s.is_empty() {
            f.any_prefer_provider = Some(s);
        }
    }
    if let Some(v) = header_list(headers, "x-only-providers") {
        f.only_providers = Some(v.into_iter().map(|s| s.to_lowercase()).collect());
    }
    if let Some(v) = header_list(headers, "x-only-models") {
        f.only_models = Some(v);
    }
    if let Some(v) = headers.get("x-prefer-models-ignore-cooldown").and_then(|x| x.to_str().ok()) {
        let s = v.trim().to_lowercase();
        if s == "true" || s == "1" {
            f.prefer_ignore_cooldown = true;
        } else if s == "false" || s == "0" {
            f.prefer_ignore_cooldown = false;
        }
    }
    // strip from body so upstream never sees them
    if let Some(o) = body.as_object_mut() {
        for k in ["ignore_models", "prefer_models", "any_prefer_provider", "only_providers", "only_models", "prefer_models_ignore_cooldown"] {
            o.remove(k);
        }
    }
    f
}

fn parse_list_owned(v: &Value) -> Vec<String> {
    parse_list(Some(v))
}

// ---------------------------------------------------------------- matching

/// pattern: "provider:model" | "provider/model" | "any:x" | bare "x" (any provider)
pub fn key_matches(pattern: &str, provider: &str, model: &str) -> bool {
    let p = pattern.trim();
    if p.is_empty() {
        return false;
    }
    if let Some(rest) = p.strip_prefix("any:").or_else(|| p.strip_prefix("any/")) {
        return rest == model;
    }
    if let Some((pp, mm)) = split_ref(p) {
        return pp == provider.to_lowercase() && mm == model;
    }
    p == model
}

pub fn any_matches(patterns: &[String], provider: &str, model: &str) -> bool {
    patterns.iter().any(|p| key_matches(p, provider, model))
}

// ---------------------------------------------------------------- candidates

#[derive(Debug, Clone)]
pub struct Candidate {
    pub provider: String,
    pub model: String,
    pub member_key: Option<String>,
    pub preferred: bool,
}

#[derive(Debug, Clone)]
pub enum Target {
    Direct { provider: String, model: String },
    Any { model: String },
    Alias { name: String },
}

pub fn parse_target(model_field: &str, aliases: &HashMap<String, crate::config::AliasConfig>) -> Option<Target> {
    let m = model_field.trim();
    if m.is_empty() {
        return None;
    }
    if aliases.contains_key(m) {
        return Some(Target::Alias { name: m.to_string() });
    }
    if let Some(rest) = m.strip_prefix("any:").or_else(|| m.strip_prefix("any/")) {
        if rest.is_empty() {
            return None;
        }
        return Some(Target::Any { model: rest.to_string() });
    }
    split_ref(m).map(|(provider, model)| Target::Direct { provider, model })
}

/// Scan chat body for required input modalities + audio output.
pub fn required_modalities(body: &Value) -> Vec<String> {
    let mut req = vec![];
    let mut need = |s: &str| {
        if !req.contains(&s.to_string()) {
            req.push(s.to_string());
        }
    };
    if let Some(ms) = body.get("messages").and_then(|m| m.as_array()) {
        for m in ms {
            if let Some(parts) = m.get("content").and_then(|c| c.as_array()) {
                for p in parts {
                    match p.get("type").and_then(|x| x.as_str()) {
                        Some("image_url") => need("image"),
                        Some("input_audio") | Some("audio_url") => need("audio"),
                        Some("video_url") => need("video"),
                        Some("file") => need("pdf"),
                        _ => {}
                    }
                }
            }
        }
    }
    if let Some(mo) = body.get("modalities").and_then(|m| m.as_array()) {
        for x in mo {
            if x.as_str() == Some("audio") {
                need("audio");
            }
        }
    }
    if body.get("audio").is_some() {
        need("audio");
    }
    req
}

pub fn default_caps() -> ModelCaps {
    ModelCaps {
        provider: String::new(),
        id: String::new(),
        name: String::new(),
        image: false,
        audio: false,
        video: false,
        pdf: false,
        tool_call: true,
        structured: true,
        reasoning: false,
        embedding: false,
    }
}

fn member_caps(member_key: &str, provider: &str, model: &str, alias: &AliasConfig, catalog: &Catalog) -> ModelCaps {
    let mut c = catalog.get(provider, model).cloned().unwrap_or_else(default_caps);
    c.provider = provider.to_string();
    c.id = model.to_string();
    if let Some(m) = alias.models.get(member_key) {
        for (k, v) in &m.capabilities_override {
            match k.as_str() {
                "image" => c.image = *v,
                "audio" => c.audio = *v,
                "video" => c.video = *v,
                "pdf" => c.pdf = *v,
                _ => {}
            }
        }
    }
    c
}

/// Expand alias members to candidates (enabled only).
pub fn alias_candidates(alias_name: &str, cfg: &AppConfig, catalog: &Catalog) -> Vec<Candidate> {
    let alias = match cfg.aliases.get(alias_name) {
        Some(a) => a,
        None => return vec![],
    };
    let mut keys: Vec<String> = alias.models.keys().cloned().collect();
    keys.sort();
    let mut out = vec![];
    for k in keys {
        let m = &alias.models[&k];
        if !m.enabled {
            continue;
        }
        if let Some((p, id)) = split_ref(&k) {
            out.push(Candidate { provider: p, model: id, member_key: Some(k.clone()), preferred: false });
        }
    }
    let _ = catalog;
    out
}

/// Order candidates per alias pick mode (before flags/cooldown).
pub fn order_for_pick(alias: &AliasConfig, mut cands: Vec<Candidate>) -> Vec<Candidate> {
    match alias.pick_mode.as_str() {
        "custom_order" | "failover" => {
            if alias.order.is_empty() {
                return cands;
            }
            let mut ranked: Vec<(usize, Candidate)> = vec![];
            let mut rest = vec![];
            for c in cands.drain(..) {
                let key = c.member_key.clone().unwrap_or_else(|| format!("{}:{}", c.provider, c.model));
                if let Some(i) = alias.order.iter().position(|o| o == &key || key_matches(o, &c.provider, &c.model)) {
                    ranked.push((i, c));
                } else {
                    rest.push(c);
                }
            }
            ranked.sort_by_key(|(i, _)| *i);
            ranked.into_iter().map(|(_, c)| c).chain(rest).collect()
        }
        _ => cands, // round_robin handled at pick time; classifier resolves winner first
    }
}

pub fn apply_flags(mut cands: Vec<Candidate>, f: &Flags) -> Vec<Candidate> {
    // only_* intersection
    if let Some(ops) = &f.only_providers {
        cands.retain(|c| ops.iter().any(|o| o == &c.provider.to_lowercase()));
    }
    if let Some(oms) = &f.only_models {
        cands.retain(|c| any_matches(oms, &c.provider, &c.model));
    }
    // ignore
    if !f.ignore.is_empty() {
        cands.retain(|c| !any_matches(&f.ignore, &c.provider, &c.model));
    }
    // any_prefer_provider boost (stable: keep relative order otherwise)
    if let Some(p) = &f.any_prefer_provider {
        let mut hit = vec![];
        let mut rest = vec![];
        for c in cands.drain(..) {
            if &c.provider.to_lowercase() == p {
                hit.push(c);
            } else {
                rest.push(c);
            }
        }
        // mark preferred
        for c in hit.iter_mut() {
            c.preferred = true;
        }
        hit.extend(rest);
        cands = hit;
    }
    // prefer_models reorder (wins over any_prefer boost)
    if !f.prefer.is_empty() {
        let mut ranked: Vec<(usize, Candidate)> = vec![];
        let mut rest = vec![];
        for mut c in cands.drain(..) {
            if let Some(i) = f.prefer.iter().position(|p| key_matches(p, &c.provider, &c.model)) {
                c.preferred = true;
                ranked.push((i, c));
            } else {
                rest.push(c);
            }
        }
        ranked.sort_by_key(|(i, _)| *i);
        ranked.into_iter().map(|(_, c)| c).chain(rest).collect()
    } else {
        cands
    }
}

pub fn filter_modalities(cands: Vec<Candidate>, required: &[String], alias: Option<&AliasConfig>, catalog: &Catalog) -> Vec<Candidate> {
    if required.is_empty() {
        return cands;
    }
    cands.into_iter().filter(|c| {
        let caps = match (&c.member_key, alias) {
            (Some(k), Some(a)) => member_caps(k, &c.provider, &c.model, a, catalog),
            _ => catalog.get(&c.provider, &c.model).cloned().unwrap_or_else(default_caps),
        };
        required.iter().all(|r| match r.as_str() {
            "image" => caps.image,
            "audio" => caps.audio,
            "video" => caps.video,
            "pdf" => caps.pdf,
            _ => true,
        })
    }).collect()
}

// ---------------------------------------------------------------- cooldowns

pub fn cd_key(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

pub fn is_cooled(st: &Shared, key: &str) -> bool {
    if let Some(exp) = st.cooldowns.get(key) {
        if *exp > Instant::now() {
            return true;
        }
        st.cooldowns.remove(key);
    }
    false
}

pub fn cool(st: &Shared, key: &str) {
    st.cooldowns.insert(key.to_string(), Instant::now() + Duration::from_secs(60));
}

pub fn apply_cooldown(mut cands: Vec<Candidate>, st: &Shared, flags: &Flags) -> Vec<Candidate> {
    cands.retain(|c| {
        let k = cd_key(&c.provider, &c.model);
        if !is_cooled(st, &k) {
            return true;
        }
        // preferred picks bypass cooldown when allowed
        c.preferred && flags.prefer_ignore_cooldown
    });
    cands
}

// ---------------------------------------------------------------- picking

/// Round-robin pick index for a key.
pub fn rr_pick(st: &Shared, key: &str, n: usize) -> usize {
    let e = st.rr.entry(key.to_string()).or_insert_with(|| std::sync::atomic::AtomicU64::new(0));
    (e.fetch_add(1, Ordering::Relaxed) as usize) % n.max(1)
}

pub fn alias_kind(models: &std::collections::HashMap<String, crate::config::AliasModelConfig>, catalog: &Catalog) -> &'static str {
    // decision membership is determined by decision catalog at proxy time;
    // here: embedding if all members look like embeddings (heuristic), else chat.
    if models.is_empty() {
        return "chat";
    }
    let mut emb = 0;
    let mut n = 0;
    for k in models.keys() {
        if let Some((p, id)) = split_ref(k) {
            n += 1;
            let c = catalog.get(&p, &id);
            let is_emb = c.map(|c| c.embedding).unwrap_or_else(|| {
                let l = id.to_lowercase();
                l.contains("embed")
            });
            if is_emb {
                emb += 1;
            }
        }
    }
    if n > 0 && emb == n {
        "embedding"
    } else {
        "chat"
    }
}

pub fn alias_kind_full(alias_name: &str, cfg: &AppConfig, catalog: &Catalog, decision_member: &dyn Fn(&str) -> bool) -> &'static str {
    let alias = match cfg.aliases.get(alias_name) {
        Some(a) => a,
        None => return "chat",
    };
    if alias.models.is_empty() {
        return "chat";
    }
    let mut dec = 0;
    let mut emb = 0;
    let mut n = 0;
    for k in alias.models.keys() {
        if let Some((p, id)) = split_ref(k) {
            n += 1;
            if decision_member(k) {
                dec += 1;
                continue;
            }
            let c = catalog.get(&p, &id);
            let is_emb = c.map(|c| c.embedding).unwrap_or_else(|| id.to_lowercase().contains("embed"));
            if is_emb {
                emb += 1;
            }
        }
    }
    if n > 0 && dec == n {
        "decision"
    } else if n > 0 && emb == n {
        "embedding"
    } else {
        "chat"
    }
}
