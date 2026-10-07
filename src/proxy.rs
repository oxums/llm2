use crate::catalog::Catalog;
use crate::classifier;
use crate::config::AppConfig;
use crate::providers::{self, TranslatedKind};
use crate::router::*;
use crate::state::{err, Shared};
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Instant;

// ---------------------------------------------------------------- auth

pub async fn authorized(st: &Shared, headers: &HeaderMap) -> bool {
    let want = st.api_key.read().await.clone();
    if let Some(h) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(tok) = h.strip_prefix("Bearer ") {
            return tok == want;
        }
    }
    if let Some(h) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return h == want;
    }
    false
}

fn record(st: &Shared, t0: Instant, ok: bool, usage: Option<&Value>) {
    st.metrics.requests.fetch_add(1, Ordering::Relaxed);
    if !ok {
        st.metrics.errors.fetch_add(1, Ordering::Relaxed);
    }
    st.metrics.lat_sum_ms.fetch_add(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
    st.metrics.lat_count.fetch_add(1, Ordering::Relaxed);
    if let Some(u) = usage {
        if let Some(p) = u.get("prompt_tokens").and_then(|x| x.as_u64()).or_else(|| u.get("input_tokens").and_then(|x| x.as_u64())) {
            st.metrics.tokens_in.fetch_add(p, Ordering::Relaxed);
        }
        if let Some(c) = u.get("completion_tokens").and_then(|x| x.as_u64()).or_else(|| u.get("output_tokens").and_then(|x| x.as_u64())) {
            st.metrics.tokens_out.fetch_add(c, Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------- /v1/models

pub async fn models(State(st): State<Shared>, headers: HeaderMap, q: axum::extract::Query<HashMap<String, String>>) -> impl axum::response::IntoResponse {
    if !authorized(&st, &headers).await {
        return err(StatusCode::UNAUTHORIZED, "auth", "missing or invalid api key").into_response();
    }
    let mode = q.get("output_modalities").map(|s| s.as_str()).unwrap_or("text");
    let want_dec = mode == "decisions" || mode == "all";
    let want_text = mode == "text" || mode == "all" || mode.split(',').any(|x| ["text", "image", "audio", "video"].contains(&x.trim()));
    let cfg = st.config.read().await;
    let cat = st.catalog.read().await;
    let mut data = vec![];
    if want_text {
        let mut seen_any = std::collections::HashSet::new();
        for c in cat.models.values() {
            if c.embedding {
                continue;
            }
            // respect provider enabled_models for exposure
            if let Some(pc) = cfg.providers.get(&c.provider) {
                if let Some(allow) = &pc.enabled_models {
                    if !allow.iter().any(|m| m == &c.id) {
                        continue;
                    }
                }
            }
            data.push(serde_json::json!({"id": format!("{}/{}", c.provider, c.id), "object": "model", "owned_by": c.provider}));
            if seen_any.insert(c.id.clone()) {
                data.push(serde_json::json!({"id": format!("any/{}", c.id), "object": "model", "owned_by": "any"}));
            }
        }
        for (name, a) in &cfg.aliases {
            let kind = alias_kind(&a.models, &cat);
            if kind == "chat" {
                data.push(serde_json::json!({"id": name, "object": "model", "owned_by": "alias"}));
            }
        }
    }
    if want_dec {
        for d in &cat.decisions {
            data.push(serde_json::json!({"id": format!("{}/{}", d.source, d.slug), "object": "model", "owned_by": d.source}));
        }
        for (name, a) in &cfg.aliases {
            if alias_kind_full(name, &cfg, &cat, &|k| is_decision_member(&cat, k)) == "decision" {
                data.push(serde_json::json!({"id": name, "object": "model", "owned_by": "alias"}));
            }
        }
    }
    axum::Json(serde_json::json!({"object": "list", "data": data})).into_response()
}

fn is_decision_member(cat: &Catalog, member_key: &str) -> bool {
    // member "openrouter/cloudflare/clef" or "typesafe/jev-latest"
    if let Some(rest) = member_key.strip_prefix("openrouter/") {
        return cat.decisions.iter().any(|d| d.source == "openrouter" && d.slug == rest);
    }
    if let Some(rest) = member_key.strip_prefix("typesafe/") {
        let full = format!("typesafe/{rest}");
        return cat.decisions.iter().any(|d| d.source == "typesafe" && d.slug == full);
    }
    false
}

// ---------------------------------------------------------------- chat

pub async fn chat(State(st): State<Shared>, headers: HeaderMap, body: Json<Value>) -> impl axum::response::IntoResponse {
    use axum::response::IntoResponse;
    let t0 = Instant::now();
    st.metrics.chat.fetch_add(1, Ordering::Relaxed);
    if !authorized(&st, &headers).await {
        record(&st, t0, false, None);
        return err(StatusCode::UNAUTHORIZED, "auth", "missing or invalid api key").into_response();
    }
    let mut b = body.0;
    let model_field = b.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    if model_field.is_empty() {
        record(&st, t0, false, None);
        return err(StatusCode::BAD_REQUEST, "no_model", "missing model field").into_response();
    }
    let flags = extract_flags(&mut b, &headers);
    let stream = b.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    let cfg = st.config.read().await.clone();
    let cat = st.catalog.read().await.clone();
    let target = match parse_target(&model_field, &cfg.aliases) {
        Some(t) => t,
        None => {
            record(&st, t0, false, None);
            return err(StatusCode::BAD_REQUEST, "bad_model", format!("unparseable model '{model_field}'")).into_response();
        }
    };

    // build candidate list
    let required = required_modalities(&b);
    let (mut cands, alias_cfg, routed_alias): (Vec<Candidate>, Option<crate::config::AliasConfig>, Option<String>) = match &target {
        Target::Direct { provider, model } => {
            if !cfg.providers.contains_key(provider) && crate::providers::default_base(provider).is_none() {
                record(&st, t0, false, None);
                return err(StatusCode::BAD_REQUEST, "unknown_provider", format!("unknown provider '{provider}'")).into_response();
            }
            // enabled_models gate
            if let Some(pc) = cfg.providers.get(provider) {
                if let Some(allow) = &pc.enabled_models {
                    if !allow.iter().any(|m| m == model) {
                        record(&st, t0, false, None);
                        return err(StatusCode::FORBIDDEN, "model_not_allowed", format!("model '{model}' not enabled for {provider}")).into_response();
                    }
                }
            }
            let c = Candidate { provider: provider.clone(), model: model.clone(), member_key: None, preferred: false };
            // modality gate for direct too
            let kept = filter_modalities(vec![c], &required, None, &cat);
            if kept.is_empty() {
                record(&st, t0, false, None);
                return err(StatusCode::BAD_REQUEST, "no_model_supports_modality", "direct model lacks required modality (image/audio/video/pdf)").into_response();
            }
            (kept, None, None)
        }
        Target::Any { model } => {
            let mut provs = cat.providers_for(model);
            if provs.is_empty() {
                // unknown to catalog: try every linked provider (they may serve it)
                provs = cfg.providers.iter().filter(|(_, p)| p.is_linked()).map(|(k, _)| k.clone()).collect();
                provs.sort();
            }
            let mut c: Vec<Candidate> = provs.into_iter().map(|p| Candidate { provider: p, model: model.clone(), member_key: None, preferred: false }).collect();
            // keep only linked providers + enabled allow
            c.retain(|x| {
                if let Some(pc) = cfg.providers.get(&x.provider) {
                    if !pc.is_linked() && x.provider != "ollama" && x.provider != "lmstudio" && !matches!(x.provider.as_str(), "openai-compatible") {
                        // allow keyless local providers; others need key
                        if crate::providers::default_base(&x.provider).map(|u| u.contains("localhost") || u.contains("127.0.0.1")).unwrap_or(false) {
                            return true;
                        }
                        return false;
                    }
                    if let Some(allow) = &pc.enabled_models {
                        return allow.iter().any(|m| m == &x.model);
                    }
                    return true;
                }
                false
            });
            c = apply_flags(c, &flags);
            c = filter_modalities(c, &required, None, &cat);
            c = apply_cooldown(c, &st, &flags);
            if c.is_empty() {
                record(&st, t0, false, None);
                return err(StatusCode::NOT_FOUND, "no_provider", format!("no available provider for model '{model}'")).into_response();
            }
            // rotate start for load balancing
            let idx = rr_pick(&st, &format!("any:{model}"), c.len());
            c.rotate_left(idx);
            (c, None, None)
        }
        Target::Alias { name } => {
            let a = cfg.aliases.get(name).cloned().unwrap();
            let kind = alias_kind_full(name, &cfg, &cat, &|k| is_decision_member(&cat, k));
            if kind != "chat" {
                record(&st, t0, false, None);
                return err(StatusCode::BAD_REQUEST, "wrong_alias_kind", format!("alias '{name}' is {kind}-only")).into_response();
            }
            let mut c = alias_candidates(name, &cfg, &cat);
            c = order_for_pick(&a, c);
            c = apply_flags(c, &flags);
            c = filter_modalities(c, &required, Some(&a), &cat);
            c = apply_cooldown(c, &st, &flags);
            if c.is_empty() {
                record(&st, t0, false, None);
                return err(StatusCode::BAD_REQUEST, "no_models_after_filter", "no alias members left after flags/modality/cooldown filters").into_response();
            }
            if a.pick_mode == "round_robin" {
                let idx = rr_pick(&st, name, c.len());
                c.rotate_left(idx);
                (c, Some(a), Some(name.clone()))
            } else if a.pick_mode == "classifier" {
                let r = classifier::classify(&st, name, &a, c.clone(), &b).await;
                let mut ordered = vec![r.winner.clone()];
                let wk = r.winner.member_key.clone();
                for x in c {
                    if x.member_key != wk {
                        ordered.push(x);
                    }
                }
                // stash routing info in a response header later
                (ordered, Some(a), Some(format!("{name}|conf={:.2}|via={}", r.confidence, r.via)))
            } else {
                (c, Some(a), Some(name.clone()))
            }
        }
    };

    // send loop with failover
    let mut last_err: Option<(StatusCode, String)> = None;
    let mut tried_cooled_retry = false;
    let mut queue = cands;
    loop {
        let mut progressed = false;
        let mut i = 0;
        while i < queue.len() {
            let c = queue[i].clone();
            progressed = true;
            let pcfg = match cfg.providers.get(&c.provider) {
                Some(p) => p.clone(),
                None => {
                    // provider not configured: treat as failed candidate
                    cool(&st, &cd_key(&c.provider, &c.model));
                    i += 1;
                    continue;
                }
            };
            // member-level overrides
            let (mheaders, mbody) = member_overrides(&alias_cfg, &c);
            // merge provider keyless check
            let disp = match providers::build_chat(&st.http, &c.provider, &pcfg, &c.model, b.clone(), &mheaders, &mbody) {
                Ok(d) => d,
                Err(e) => {
                    cool(&st, &cd_key(&c.provider, &c.model));
                    last_err = Some((StatusCode::BAD_GATEWAY, e));
                    i += 1;
                    continue;
                }
            };
            let kind = disp.translated;
            let req_model_disp = model_field.clone();
            let send = disp.req.send().await;
            match send {
                Err(e) => {
                    cool(&st, &cd_key(&c.provider, &c.model));
                    last_err = Some((StatusCode::BAD_GATEWAY, format!("upstream transport: {e}")));
                    i += 1;
                    continue;
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if !(200..300).contains(&status) {
                        let body_txt = resp.text().await.unwrap_or_default();
                        if providers::is_retryable(status) {
                            cool(&st, &cd_key(&c.provider, &c.model));
                            last_err = Some((StatusCode::from_u16(status.min(599)).unwrap_or(StatusCode::BAD_GATEWAY), body_txt));
                            i += 1;
                            continue;
                        } else {
                            record(&st, t0, false, None);
                            let code = StatusCode::from_u16(status.min(599)).unwrap_or(StatusCode::BAD_GATEWAY);
                            let mut r = Body::from(body_txt).into_response();
                            *r.status_mut() = code;
                            r.headers_mut().insert("content-type", "application/json".parse().unwrap());
                            return r;
                        }
                    }
                    // success
                    let routed = format!("{}/{}", c.provider, c.model);
                    if stream {
                        let s = resp.bytes_stream();
                        let body_stream: Body = if kind == TranslatedKind::None {
                            Body::from_stream(s)
                        } else {
                            Body::from_stream(providers::translated_stream(kind, req_model_disp, s))
                        };
                        record(&st, t0, true, None);
                        let mut r = body_stream.into_response();
                        *r.status_mut() = StatusCode::OK;
                        let h = r.headers_mut();
                        h.insert("content-type", "text/event-stream".parse().unwrap());
                        h.insert("cache-control", "no-cache".parse().unwrap());
                        if let Ok(v) = routed.parse::<axum::http::HeaderValue>() {
                            h.insert("x-routed-by", v);
                        }
                        return r;
                    } else if kind == TranslatedKind::None {
                        let bytes = resp.bytes().await.unwrap_or_default();
                        let usage = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v.get("usage").cloned());
                        record(&st, t0, true, usage.as_ref());
                        let mut r = Body::from(bytes).into_response();
                        *r.status_mut() = StatusCode::OK;
                        let h = r.headers_mut();
                        h.insert("content-type", "application/json".parse().unwrap());
                        if let Ok(v) = routed.parse::<axum::http::HeaderValue>() {
                            h.insert("x-routed-by", v);
                        }
                        return r;
                    } else {
                        let v: Value = resp.json().await.unwrap_or(Value::Null);
                        let out = if kind == TranslatedKind::Anthropic {
                            providers::from_anthropic(&v, &req_model_disp)
                        } else {
                            providers::from_google(&v, &req_model_disp)
                        };
                        let usage = out.get("usage").cloned();
                        record(&st, t0, true, usage.as_ref());
                        let _ = routed_alias;
                        let mut r = Json(out).into_response();
                        if let Ok(v) = routed.parse::<axum::http::HeaderValue>() {
                            r.headers_mut().insert("x-routed-by", v);
                        }
                        return r;
                    }
                }
            }
        }
        if !progressed {
            break;
        }
        // all failed. If cooldowns may be blocking others, do one immediate full retry.
        if !tried_cooled_retry {
            tried_cooled_retry = true;
            // rebuild full queue ignoring cooldown (except preferred-bypass already applied)
            queue = match &target {
                Target::Alias { name } => {
                    let a = cfg.aliases.get(name).cloned().unwrap();
                    let mut c = alias_candidates(name, &cfg, &cat);
                    c = order_for_pick(&a, c);
                    c = apply_flags(c, &flags);
                    c = filter_modalities(c, &required, Some(&a), &cat);
                    c
                }
                _ => break,
            };
            if queue.is_empty() {
                break;
            }
            continue;
        }
        break;
    }
    record(&st, t0, false, None);
    let (code, msg) = last_err.unwrap_or((StatusCode::BAD_GATEWAY, "all providers failed".into()));
    err(code, "all_failed", msg).into_response()
}

fn member_overrides(alias: &Option<crate::config::AliasConfig>, c: &Candidate) -> (HashMap<String, String>, Value) {
    let mut h = HashMap::new();
    let mut b = Value::Null;
    if let (Some(a), Some(k)) = (alias, &c.member_key) {
        if let Some(m) = a.models.get(k) {
            h = m.extra_headers.clone();
            b = m.extra_body.clone();
        }
    }
    (h, b)
}

// ---------------------------------------------------------------- embeddings

pub async fn embeddings(State(st): State<Shared>, headers: HeaderMap, body: Json<Value>) -> impl axum::response::IntoResponse {
    use axum::response::IntoResponse;
    let t0 = Instant::now();
    st.metrics.embeddings.fetch_add(1, Ordering::Relaxed);
    if !authorized(&st, &headers).await {
        record(&st, t0, false, None);
        return err(StatusCode::UNAUTHORIZED, "auth", "missing or invalid api key").into_response();
    }
    let mut b = body.0;
    let model_field = b.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let flags = extract_flags(&mut b, &headers);
    let cfg = st.config.read().await.clone();
    let cat = st.catalog.read().await.clone();
    let target = match parse_target(&model_field, &cfg.aliases) {
        Some(t) => t,
        None => {
            record(&st, t0, false, None);
            return err(StatusCode::BAD_REQUEST, "bad_model", "unparseable model").into_response();
        }
    };
    let mut cands: Vec<Candidate> = match &target {
        Target::Direct { provider, model } => {
            if !cfg.providers.contains_key(provider) && providers::default_base(provider).is_none() {
                record(&st, t0, false, None);
                return err(StatusCode::BAD_REQUEST, "unknown_provider", format!("unknown provider '{provider}'")).into_response();
            }
            vec![Candidate { provider: provider.clone(), model: model.clone(), member_key: None, preferred: false }]
        }
        Target::Any { model } => {
            let mut provs: Vec<String> = cat.models.values().filter(|c| c.embedding && c.id == *model).map(|c| c.provider.clone()).collect();
            provs.sort();
            provs.dedup();
            if provs.is_empty() {
                provs = cfg.providers.iter().filter(|(_, p)| p.is_linked()).map(|(k, _)| k.clone()).collect();
                provs.sort();
            }
            let mut c: Vec<Candidate> = provs.into_iter().map(|p| Candidate { provider: p, model: model.clone(), member_key: None, preferred: false }).collect();
            c.retain(|x| cfg.providers.get(&x.provider).map(|p| p.is_linked()).unwrap_or(false));
            c = apply_flags(c, &flags);
            c = apply_cooldown(c, &st, &flags);
            c
        }
        Target::Alias { name } => {
            let a = cfg.aliases.get(name).cloned().unwrap();
            if alias_kind_full(name, &cfg, &cat, &|k| is_decision_member(&cat, k)) != "embedding" {
                record(&st, t0, false, None);
                return err(StatusCode::BAD_REQUEST, "wrong_alias_kind", "alias is not embedding-only").into_response();
            }
            let mut c = alias_candidates(name, &cfg, &cat);
            c = order_for_pick(&a, c);
            c = apply_flags(c, &flags);
            c = apply_cooldown(c, &st, &flags);
            if a.pick_mode == "round_robin" && !c.is_empty() {
                let idx = rr_pick(&st, name, c.len());
                c.rotate_left(idx);
            }
            c
        }
    };
    // keep only embedding-capable for safety on direct refs known to chat;
    // unknown models are attempted (provider may serve them).
    cands.retain(|c| {
        if c.member_key.is_some() {
            return true; // alias kind already validated
        }
        match cat.get(&c.provider, &c.model) {
            Some(m) => m.embedding,
            None => true,
        }
    });
    if cands.is_empty() {
        record(&st, t0, false, None);
        return err(StatusCode::BAD_REQUEST, "no_models_after_filter", "no embedding candidates").into_response();
    }
    let mut last = (StatusCode::BAD_GATEWAY, "all failed".to_string());
    for c in &cands {
        let pcfg = match cfg.providers.get(&c.provider) {
            Some(p) => p.clone(),
            None => continue,
        };
        if matches!(c.provider.as_str(), "anthropic" | "google" | "typesafe") {
            last = (StatusCode::BAD_REQUEST, format!("provider {} has no embeddings endpoint in v1", c.provider));
            continue;
        }
        let (mheaders, mbody) = member_overrides(&cfg.aliases.get("").cloned(), c);
        let mut eb = providers::merge_body(b.clone(), &mbody);
        eb["model"] = Value::String(c.model.clone());
        let base = match providers::base_url(&c.provider, &pcfg) {
            Ok(u) => u,
            Err(e) => {
                last = (StatusCode::BAD_GATEWAY, e);
                continue;
            }
        };
        let url = if c.provider == "azure-openai" {
            let ver = pcfg.api_version.clone().unwrap_or_else(|| "2024-10-01-preview".into());
            format!("{}/openai/deployments/{}/embeddings?api-version={}", base.trim_end_matches('/'), c.model, ver)
        } else {
            format!("{}/embeddings", base)
        };
        let mut r = st.http.post(&url).json(&eb);
        if !pcfg.api_key.is_empty() {
            if c.provider == "azure-openai" {
                r = r.header("api-key", pcfg.api_key.clone());
            } else {
                r = r.bearer_auth(pcfg.api_key.clone());
            }
        }
        for (k, v) in pcfg.extra_headers.iter().chain(mheaders.iter()) {
            r = r.header(k.as_str(), v.as_str());
        }
        match r.send().await {
            Err(e) => {
                cool(&st, &cd_key(&c.provider, &c.model));
                last = (StatusCode::BAD_GATEWAY, format!("transport: {e}"));
            }
            Ok(resp) => {
                let s = resp.status().as_u16();
                let bytes = resp.bytes().await.unwrap_or_default();
                if (200..300).contains(&s) {
                    let usage = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v.get("usage").cloned());
                    record(&st, t0, true, usage.as_ref());
                    let mut r = Body::from(bytes).into_response();
                    *r.status_mut() = StatusCode::OK;
                    r.headers_mut().insert("content-type", "application/json".parse().unwrap());
                    return r;
                }
                let txt = String::from_utf8_lossy(&bytes).to_string();
                if providers::is_retryable(s) {
                    cool(&st, &cd_key(&c.provider, &c.model));
                    last = (StatusCode::from_u16(s.min(599)).unwrap(), txt);
                } else {
                    record(&st, t0, false, None);
                    let mut r = Body::from(bytes).into_response();
                    *r.status_mut() = StatusCode::from_u16(s.min(599)).unwrap();
                    r.headers_mut().insert("content-type", "application/json".parse().unwrap());
                    return r;
                }
            }
        }
    }
    record(&st, t0, false, None);
    err(last.0, "all_failed", last.1).into_response()
}

// ---------------------------------------------------------------- decisions

pub async fn decisions(State(st): State<Shared>, headers: HeaderMap, body: Json<Value>) -> impl axum::response::IntoResponse {
    use axum::response::IntoResponse;
    let t0 = Instant::now();
    st.metrics.decisions.fetch_add(1, Ordering::Relaxed);
    if !authorized(&st, &headers).await {
        record(&st, t0, false, None);
        return err(StatusCode::UNAUTHORIZED, "auth", "missing or invalid api key").into_response();
    }
    let mut b = body.0;
    let model_field = b.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let flags = extract_flags(&mut b, &headers);
    let state_val = b.get("state").cloned().unwrap_or(Value::Null);
    let questions = match b.get("questions").cloned() {
        Some(q) => q,
        None => {
            record(&st, t0, false, None);
            return err(StatusCode::BAD_REQUEST, "no_questions", "missing questions").into_response();
        }
    };
    let provider_prefs = b.get("provider").cloned();
    let cfg = st.config.read().await.clone();
    let cat = st.catalog.read().await.clone();

    // candidates: (source, slug)
    let mut cands: Vec<(String, String, Option<String>, bool)> = vec![]; // source, slug, member_key, preferred
    if let Some(a) = cfg.aliases.get(&model_field) {
        if alias_kind_full(&model_field, &cfg, &cat, &|k| is_decision_member(&cat, k)) != "decision" {
            record(&st, t0, false, None);
            return err(StatusCode::BAD_REQUEST, "wrong_alias_kind", "alias is not decision-only").into_response();
        }
        let mut keys: Vec<String> = a.models.keys().cloned().collect();
        keys.sort();
        // order
        let mut ordered_keys = vec![];
        if !a.order.is_empty() {
            for o in &a.order {
                if keys.contains(o) {
                    ordered_keys.push(o.clone());
                }
            }
            for k in &keys {
                if !ordered_keys.contains(k) {
                    ordered_keys.push(k.clone());
                }
            }
        } else {
            ordered_keys = keys;
        }
        for k in ordered_keys {
            if !a.models.get(&k).map(|m| m.enabled).unwrap_or(true) {
                continue;
            }
            if let Some((s, slug)) = decision_source_slug(&k, &cat) {
                cands.push((s, slug, Some(k), false));
            }
        }
        // flags on (source-as-provider, slug)
        cands = apply_dec_flags(cands, &flags);
        cands = apply_dec_cooldown(cands, &st, &flags);
        if a.pick_mode == "round_robin" && !cands.is_empty() {
            let idx = rr_pick(&st, &model_field, cands.len());
            cands.rotate_left(idx);
        }
    } else {
        // direct: "openrouter/cloudflare/clef" | "typesafe/jev-latest" | bare catalog slug | "any/slug"
        let m = model_field.strip_prefix("any:").or_else(|| model_field.strip_prefix("any/")).unwrap_or(&model_field);
        // expand: every linked source serving it
        let mut opts: Vec<(String, String)> = vec![];
        if let Some(rest) = m.strip_prefix("openrouter/") {
            opts.push(("openrouter".into(), rest.to_string()));
        } else if let Some(rest) = m.strip_prefix("typesafe/") {
            let full = format!("typesafe/{rest}");
            opts.push(("typesafe".into(), full.clone()));
            // same slug via openrouter if cataloged
            if cat.decisions.iter().any(|d| d.source == "openrouter" && (d.slug == full || d.slug == rest)) {
                let slug = if cat.decisions.iter().any(|d| d.source == "openrouter" && d.slug == full) { full } else { rest.to_string() };
                opts.push(("openrouter".into(), slug));
            }
        } else if cat.decisions.iter().any(|d| d.slug == m) {
            for d in cat.decisions.iter().filter(|d| d.slug == m) {
                opts.push((d.source.clone(), d.slug.clone()));
            }
        } else {
            record(&st, t0, false, None);
            return err(StatusCode::NOT_FOUND, "unknown_decision_model", format!("unknown decision model '{model_field}'")).into_response();
        }
        // keep linked only
        opts.retain(|(s, _)| {
            if s == "typesafe" {
                cfg.providers.get("typesafe").map(|p| p.is_linked()).unwrap_or(false)
            } else {
                cfg.providers.get("openrouter").map(|p| p.is_linked()).unwrap_or(false)
            }
        });
        for (s, slug) in opts {
            cands.push((s, slug, None, false));
        }
        cands = apply_dec_flags(cands, &flags);
        cands = apply_dec_cooldown(cands, &st, &flags);
    }
    if cands.is_empty() {
        record(&st, t0, false, None);
        return err(StatusCode::BAD_REQUEST, "no_models_after_filter", "no decision candidates").into_response();
    }

    let tcfg = cfg.providers.get("typesafe").cloned();
    let ocfg = cfg.providers.get("openrouter").cloned();
    let mut last = (StatusCode::BAD_GATEWAY, "all failed".to_string());
    for (source, slug, _mk, _pref) in &cands {
        match providers::send_decision(&st.http, source, slug, &state_val, &questions, tcfg.as_ref(), ocfg.as_ref(), provider_prefs.as_ref(), 30000).await {
            Ok(mut v) => {
                record(&st, t0, true, None);
                v["proxy"] = serde_json::json!({"via": format!("{source}/{slug}")});
                return (StatusCode::OK, Json(v)).into_response();
            }
            Err((s, e)) => {
                if providers::is_retryable(s) || s == 504 {
                    cool(&st, &cd_key(source, slug));
                    last = (StatusCode::from_u16(s.min(599)).unwrap(), e);
                } else {
                    record(&st, t0, false, None);
                    return err(StatusCode::from_u16(s.min(599)).unwrap(), "upstream", e).into_response();
                }
            }
        }
    }
    record(&st, t0, false, None);
    err(last.0, "all_failed", last.1).into_response()
}

fn decision_source_slug(member_key: &str, cat: &Catalog) -> Option<(String, String)> {
    if let Some(rest) = member_key.strip_prefix("openrouter/") {
        if cat.decisions.iter().any(|d| d.source == "openrouter" && d.slug == rest) {
            return Some(("openrouter".into(), rest.to_string()));
        }
        // allow even if catalog hasn't refreshed (explicit admin config)
        return Some(("openrouter".into(), rest.to_string()));
    }
    if let Some(rest) = member_key.strip_prefix("typesafe/") {
        return Some(("typesafe".into(), format!("typesafe/{rest}")));
    }
    // bare slug -> catalog source
    cat.decisions.iter().find(|d| d.slug == member_key).map(|d| (d.source.clone(), d.slug.clone()))
}

fn apply_dec_flags(mut cands: Vec<(String, String, Option<String>, bool)>, f: &Flags) -> Vec<(String, String, Option<String>, bool)> {
    if let Some(ops) = &f.only_providers {
        cands.retain(|(s, _, _, _)| ops.iter().any(|o| o == &s.to_lowercase()));
    }
    if let Some(oms) = &f.only_models {
        cands.retain(|(s, slug, _, _)| any_matches(oms, s, slug));
    }
    if !f.ignore.is_empty() {
        cands.retain(|(s, slug, _, _)| !any_matches(&f.ignore, s, slug));
    }
    if !f.prefer.is_empty() {
        let mut ranked: Vec<(usize, (String, String, Option<String>, bool))> = vec![];
        let mut rest = vec![];
        for mut c in cands.drain(..) {
            if let Some(i) = f.prefer.iter().position(|p| key_matches(p, &c.0, &c.1)) {
                c.3 = true;
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

fn apply_dec_cooldown(cands: Vec<(String, String, Option<String>, bool)>, st: &Shared, f: &Flags) -> Vec<(String, String, Option<String>, bool)> {
    cands.into_iter().filter(|(s, slug, _, pref)| {
        if !is_cooled(st, &cd_key(s, slug)) {
            return true;
        }
        *pref && f.prefer_ignore_cooldown
    }).collect()
}

// ---------------------------------------------------------------- health

pub async fn health(State(st): State<Shared>) -> Json<Value> {
    let cat = st.catalog.read().await;
    Json(serde_json::json!({
        "ok": true,
        "models": cat.models.len(),
        "decision_models": cat.decisions.len(),
        "fetched_at": cat.fetched_at,
        "metrics": st.metrics.snapshot(),
    }))
}
