use crate::config::AliasConfig;
use crate::providers;
use crate::router::Candidate;
use crate::state::Shared;
use serde_json::Value;

/// Collapse chat messages to plain text state for decision models.
/// Multimodal parts become placeholders (decision models are text-state).
pub fn messages_to_state(body: &Value, max_chars: usize) -> Value {
    let mut parts: Vec<String> = vec![];
    if let Some(ms) = body.get("messages").and_then(|m| m.as_array()) {
        for m in ms {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            if role == "tool" {
                continue;
            }
            let c = m.get("content").unwrap_or(&Value::Null);
            let t = match c {
                Value::String(s) => s.clone(),
                Value::Array(ps) => ps.iter().map(|p| match p.get("type").and_then(|x| x.as_str()) {
                    Some("text") => p.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    Some("image_url") => "[image attached]".to_string(),
                    Some("input_audio") | Some("audio_url") => "[audio attached]".to_string(),
                    Some("video_url") => "[video attached]".to_string(),
                    Some("file") => "[file attached]".to_string(),
                    _ => String::new(),
                }).collect::<Vec<_>>().join(" "),
                _ => String::new(),
            };
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            parts.push(format!("{role}: {t}"));
        }
    }
    let mut s = parts.join("\n");
    if s.len() > max_chars {
        // keep the tail (most recent) — that's what matters most
        let cut = s.len() - max_chars;
        if let Some(idx) = s[cut..].find('\n') {
            s = s[cut + idx + 1..].to_string();
        } else {
            s = s[cut..].to_string();
        }
    }
    Value::String(s)
}

/// Sanitize a member key for use as a Choice option key (Clef rejects '/').
pub fn sanitize_key(member_key: &str) -> String {
    member_key.replace(['/', ':', ' '], "__")
}

pub struct ClassifyResult {
    pub winner: Candidate,
    pub confidence: f64,
    pub via: String,
}

/// Run alias classifier. Returns winner or falls back to alias failover order head.
pub async fn classify(
    st: &Shared,
    alias_name: &str,
    alias: &AliasConfig,
    mut cands: Vec<Candidate>,
    body: &Value,
) -> ClassifyResult {
    let fallback_pick = || cands.first().cloned();
    if cands.is_empty() {
        // unreachable in practice; fabricate empty
        return ClassifyResult {
            winner: Candidate { provider: String::new(), model: String::new(), member_key: None, preferred: false },
            confidence: 0.0,
            via: "none".into(),
        };
    }
    let backend = alias.classifier.backend.as_str();
    let res = if backend == "text_structured" {
        classify_text(st, alias, &cands, body).await
    } else {
        classify_jev(st, alias, &cands, body).await
    };
    match res {
        Some((idx, conf, via)) if idx < cands.len() => {
            let w = cands.remove(idx);
            ClassifyResult { winner: w, confidence: conf, via }
        }
        _ => {
            // low confidence or error -> configured fallback, else head
            let fb = alias.classifier.fallback.trim();
            if !fb.is_empty() {
                if let Some(i) = cands.iter().position(|c| {
                    let k = c.member_key.clone().unwrap_or_else(|| format!("{}:{}", c.provider, c.model));
                    k == fb || crate::router::key_matches(fb, &c.provider, &c.model)
                }) {
                    let w = cands.remove(i);
                    return ClassifyResult { winner: w, confidence: 0.0, via: "fallback".into() };
                }
            }
            ClassifyResult { winner: fallback_pick().unwrap(), confidence: 0.0, via: "fallback".into() }
        }
    }
}

fn criteria_for(cands: &[Candidate], alias: &AliasConfig) -> (Value, Vec<String>) {
    // returns (criteria map, sanitized keys in candidate order)
    let mut map = serde_json::Map::new();
    let mut keys = vec![];
    for c in cands {
        let member = c.member_key.clone().unwrap_or_else(|| format!("{}/{}", c.provider, c.model));
        let sk = sanitize_key(&member);
        let desc = alias.models.get(&member)
            .and_then(|m| m.option_description.clone())
            .unwrap_or_else(|| format!("Model {member}"));
        map.insert(sk.clone(), Value::String(desc));
        keys.push(sk);
    }
    (Value::Object(map), keys)
}

async fn classify_jev(
    st: &Shared,
    alias: &AliasConfig,
    cands: &[Candidate],
    body: &Value,
) -> Option<(usize, f64, String)> {
    let (criteria, keys) = criteria_for(cands, alias);
    let questions = serde_json::json!({
        "route": {"type": "choice", "instructions": alias.classifier.prompt, "criteria": criteria}
    });
    let state_val = messages_to_state(body, 8000);
    // resolve decision source: "typesafe/slug..." or "openrouter/slug..."
    let (source, slug) = resolve_decision_model(&alias.classifier.model)?;
    let cfg = st.config.read().await;
    let tcfg = cfg.providers.get("typesafe").cloned();
    let ocfg = cfg.providers.get("openrouter").cloned();
    let timeout = alias.classifier.timeout_ms;
    drop(cfg);
    let resp = providers::send_decision(&st.http, &source, &slug, &state_val, &questions, tcfg.as_ref(), ocfg.as_ref(), None, timeout).await.ok()?;
    let ans = resp.get("answers").and_then(|a| a.get("route"))?;
    let choice = ans.get("choice").and_then(|c| c.as_str())?;
    let conf = ans.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.0);
    let idx = keys.iter().position(|k| k == choice)?;
    if conf < alias.classifier.min_confidence {
        return None;
    }
    let model = resp.get("model").and_then(|m| m.as_str()).unwrap_or(&slug).to_string();
    Some((idx, conf, format!("{source}/{model}")))
}

/// "typesafe/jev-latest" -> ("typesafe", "typesafe/jev-latest")
/// "openrouter/cloudflare/clef" -> ("openrouter", "cloudflare/clef")
/// bare "jev-latest" -> ("typesafe", "typesafe/jev-latest")
pub fn resolve_decision_model(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix("openrouter/") {
        return Some(("openrouter".into(), rest.to_string()));
    }
    if let Some(rest) = s.strip_prefix("typesafe/") {
        return Some(("typesafe".into(), format!("typesafe/{rest}")));
    }
    // catalog slug like "cloudflare/clef" without prefix -> openrouter
    if s.contains('/') {
        return Some(("openrouter".into(), s.to_string()));
    }
    // bare jev alias
    Some(("typesafe".into(), format!("typesafe/{s}")))
}

async fn classify_text(
    st: &Shared,
    alias: &AliasConfig,
    cands: &[Candidate],
    body: &Value,
) -> Option<(usize, f64, String)> {
    // judge model is a chat ref "provider/model"
    let (jp, jm) = crate::state::split_ref(&alias.classifier.model)?;
    let cfg = st.config.read().await;
    let pcfg = cfg.providers.get(&jp).cloned()?;
    drop(cfg);
    let (criteria, _keys) = criteria_for(cands, alias);
    let options_txt = cands.iter().enumerate().map(|(i, c)| {
        let member = c.member_key.clone().unwrap_or_else(|| format!("{}/{}", c.provider, c.model));
        let d = criteria.get(&sanitize_key(&member)).and_then(|v| v.as_str()).unwrap_or("");
        format!("{i}: {member} — {d}")
    }).collect::<Vec<_>>().join("\n");
    let sys = format!("{}. Valid options:\n{}\n\nReply with a single JSON object {{\"pick\": <index>}} and nothing else.", alias.classifier.prompt, options_txt);
    let mut jb = serde_json::json!({
        "model": jm,
        "messages": [
            {"role": "system", "content": sys},
            {"role": "user", "content": messages_to_state(body, 4000)}
        ],
        "temperature": 0.0,
        "max_tokens": 64,
        "response_format": {"type": "json_object"}
    });
    // strip our flags just in case
    let disp = providers::build_chat(&st.http, &jp, &pcfg, &jm, jb, &Default::default(), &serde_json::json!({})).ok()?;
    let resp = tokio::time::timeout(std::time::Duration::from_millis(alias.classifier.timeout_ms), disp.req.send()).await.ok()?.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    let txt = v.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()).and_then(|c| c.get("message")).and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("");
    // tolerant parse: find first integer
    let num: usize = txt.chars().filter(|c| c.is_ascii_digit() || *c == '-').collect::<String>().parse().ok()?;
    if num >= cands.len() {
        return None;
    }
    Some((num, 1.0, format!("{jp}/{jm}")))
}
