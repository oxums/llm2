use crate::config::ProviderConfig;
use crate::state::Shared;
use bytes::Bytes;
use futures_util::Stream;
use std::collections::HashMap;

/// Max time to wait for upstream response headers per attempt.
/// Body streaming afterwards is unbounded (ends on client disconnect).
pub const ATTEMPT_SEND_TIMEOUT_SECS: u64 = 90;

// ---------------------------------------------------------------- base URLs

pub fn default_base(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "openai" => "https://api.openai.com/v1",
        "anthropic" => "https://api.anthropic.com/v1",
        "google" => "https://generativelanguage.googleapis.com/v1beta",
        "azure-openai" => return None, // needs user endpoint
        "mistral" => "https://api.mistral.ai/v1",
        "cohere" => "https://api.cohere.com/compatibility/v1",
        "xai" => "https://api.x.ai/v1",
        "deepseek" => "https://api.deepseek.com/v1",
        "groq" => "https://api.groq.com/openai/v1",
        "togetherai" => "https://api.together.xyz/v1",
        "fireworks" => "https://api.fireworks.ai/inference/v1",
        "deepinfra" => "https://api.deepinfra.com/v1/openai",
        "cerebras" => "https://api.cerebras.ai/v1",
        "perplexity" => "https://api.perplexity.ai",
        "openrouter" => "https://openrouter.ai/api/v1",
        "ollama" => "http://localhost:11434/v1",
        "lmstudio" => "http://localhost:1234/v1",
        "vercel" => "https://api.v0.dev/v1",
        "typesafe" => "https://api.typesafe.ai",
        _ => return None,
    })
}

pub fn base_url(provider: &str, cfg: &ProviderConfig) -> Result<String, String> {
    if let Some(b) = &cfg.base_url {
        if !b.trim().is_empty() {
            return Ok(b.trim_end_matches('/').to_string());
        }
    }
    default_base(provider)
        .map(|s| s.to_string())
        .ok_or_else(|| format!("provider '{provider}' needs base_url configured"))
}

/// Providers whose chat API is NOT OpenAI-compatible are handled by the
/// dedicated translators in build_chat (anthropic, google).
#[allow(dead_code)]
pub fn needs_translation(provider: &str) -> bool {
    matches!(provider, "anthropic" | "google")
}

pub fn is_unsupported(provider: &str) -> Option<&'static str> {
    match provider {
        "amazon-bedrock" => Some("Amazon Bedrock needs AWS SigV4 signing (not supported in v1)"),
        "google-vertex" => Some("Google Vertex needs GCP OAuth (not supported in v1)"),
        "elevenlabs" => Some("ElevenLabs is audio-only (not supported for chat in v1)"),
        _ => None,
    }
}

pub fn merge_body(mut base: serde_json::Value, extra: &serde_json::Value) -> serde_json::Value {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b[k] = v.clone();
        }
    }
    base
}

fn apply_headers(b: reqwest::RequestBuilder, extra: &HashMap<String, String>) -> reqwest::RequestBuilder {
    let mut b = b;
    for (k, v) in extra {
        b = b.header(k.as_str(), v.as_str());
    }
    b
}

// ---------------------------------------------------------------- chat send

pub struct ChatDispatch {
    pub req: reqwest::RequestBuilder,
    pub translated: TranslatedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslatedKind {
    None,
    Anthropic,
    Google,
}

pub fn build_chat(
    http: &reqwest::Client,
    provider: &str,
    cfg: &ProviderConfig,
    upstream_model: &str,
    mut body: serde_json::Value,
    extra_headers: &HashMap<String, String>,
    extra_body: &serde_json::Value,
) -> Result<ChatDispatch, String> {
    if let Some(msg) = is_unsupported(provider) {
        return Err(msg.to_string());
    }
    body = merge_body(body, extra_body);
    body["model"] = serde_json::Value::String(upstream_model.to_string());
    let stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    match provider {
        "anthropic" => {
            let key = cfg.api_key.clone();
            if key.is_empty() {
                return Err("anthropic api_key missing".into());
            }
            let url = format!("{}/messages", base_url(provider, cfg)?);
            let abody = to_anthropic(&body, upstream_model)?;
            let mut r = http.post(&url).header("x-api-key", key).header("anthropic-version", "2023-06-01").json(&abody);
            r = apply_headers(r, &cfg.extra_headers);
            r = apply_headers(r, extra_headers);
            Ok(ChatDispatch { req: r, translated: TranslatedKind::Anthropic })
        }
        "google" => {
            let key = cfg.api_key.clone();
            if key.is_empty() {
                return Err("google api_key missing".into());
            }
            let base = base_url(provider, cfg)?;
            let (url, gbody) = to_google(&body, upstream_model, &base, stream)?;
            let mut r = http.post(&url).header("x-goog-api-key", key).json(&gbody);
            r = apply_headers(r, &cfg.extra_headers);
            r = apply_headers(r, extra_headers);
            Ok(ChatDispatch { req: r, translated: TranslatedKind::Google })
        }
        "azure-openai" => {
            let ep = base_url(provider, cfg)?;
            let ver = cfg.api_version.clone().unwrap_or_else(|| "2024-10-01-preview".to_string());
            let url = format!("{}/openai/deployments/{}/chat/completions?api-version={}", ep.trim_end_matches('/'), upstream_model, ver);
            let mut r = http.post(&url);
            if !cfg.api_key.is_empty() {
                r = r.header("api-key", cfg.api_key.clone());
            }
            r = r.json(&body);
            r = apply_headers(r, &cfg.extra_headers);
            r = apply_headers(r, extra_headers);
            Ok(ChatDispatch { req: r, translated: TranslatedKind::None })
        }
        _ => {
            let base = base_url(provider, cfg)?;
            let url = format!("{}/chat/completions", base);
            // ask for a terminal usage chunk so streamed tokens can be counted
            if stream && body.get("stream_options").is_none() {
                body["stream_options"] = serde_json::json!({"include_usage": true});
            }
            let mut r = http.post(&url).json(&body);
            if !cfg.api_key.is_empty() {
                r = r.bearer_auth(cfg.api_key.clone());
            }
            if provider == "openrouter" {
                r = r.header("HTTP-Referer", "https://localhost:3000");
                r = r.header("X-Title", "llm-proxy");
            }
            r = apply_headers(r, &cfg.extra_headers);
            r = apply_headers(r, extra_headers);
            Ok(ChatDispatch { req: r, translated: TranslatedKind::None })
        }
    }
}

/// Max silence between upstream SSE chunks before the stream is closed
/// with a terminal [DONE] (bounds forever-stalled streams; normal token
/// gaps are seconds, generation can still run for hours total).
pub const STREAM_IDLE_TIMEOUT_SECS: u64 = 120;

/// Close a stalled SSE stream with [DONE] after `max_idle` of silence.
pub fn idle_guard<S, E>(upstream: S, max_idle: std::time::Duration) -> impl Stream<Item = Result<Bytes, E>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Send + 'static,
{
    futures_util::stream::unfold((Box::pin(upstream), max_idle, false), |(mut up, max_idle, mut done)| async move {
        if done {
            return None;
        }
        match tokio::time::timeout(max_idle, futures_util::StreamExt::next(&mut up)).await {
            Ok(Some(Ok(chunk))) => Some((Ok(chunk), (up, max_idle, done))),
            Ok(Some(Err(e))) => Some((Err(e), (up, max_idle, done))),
            Ok(None) => None,
            Err(_) => {
                done = true;
                Some((Ok(Bytes::from_static(b"data: [DONE]\n\n")), (up, max_idle, done)))
            }
        }
    })
}

fn find_num(s: &str, key: &str) -> Option<u64> {
    let i = s.rfind(key)?;
    let rest = &s[i + key.len()..];
    let rest = rest.trim_start_matches(|c| c == ':' || c == ' ' || c == '\t');
    let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num.is_empty() {
        None
    } else {
        num.parse().ok()
    }
}

fn scan_usage(hay: &[u8]) -> Option<(u64, u64)> {
    let s = std::str::from_utf8(hay).ok()?;
    let p = find_num(s, "\"prompt_tokens\"").or_else(|| find_num(s, "\"input_tokens\""))?;
    let c = find_num(s, "\"completion_tokens\"").or_else(|| find_num(s, "\"output_tokens\""))?;
    Some((p, c))
}

/// Wrap an SSE byte stream and credit prompt/completion tokens to metrics
/// exactly once, when a terminal usage frame passes through. Pass-through
/// otherwise (chunk sizes unchanged, small tail buffer for split frames).
pub fn tap_usage<S, E>(upstream: S, st: Shared) -> impl Stream<Item = Result<Bytes, E>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Send + 'static,
{
    futures_util::stream::unfold(
        (Box::pin(upstream), st, Vec::<u8>::new(), false),
        |(mut up, st, mut tail, mut counted)| async move {
            use std::sync::atomic::Ordering;
            match futures_util::StreamExt::next(&mut up).await {
                Some(Ok(chunk)) => {
                    if !counted {
                        let mut hay = tail.clone();
                        hay.extend_from_slice(&chunk);
                        if let Some((p, c)) = scan_usage(&hay) {
                            st.metrics.tokens_in.fetch_add(p, Ordering::Relaxed);
                            st.metrics.tokens_out.fetch_add(c, Ordering::Relaxed);
                            counted = true;
                        }
                        let keep = hay.len().saturating_sub(512);
                        tail = hay[keep..].to_vec();
                    }
                    Some((Ok(chunk), (up, st, tail, counted)))
                }
                Some(Err(e)) => Some((Err(e), (up, st, tail, counted))),
                None => None,
            }
        },
    )
}

// ---------------------------------------------------------------- anthropic

fn txt_of(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.iter().filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn to_anthropic(body: &serde_json::Value, model: &str) -> Result<serde_json::Value, String> {
    let mut system = vec![];
    let mut messages = vec![];
    if let Some(ms) = body.get("messages").and_then(|m| m.as_array()) {
        for m in ms {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = m.get("content").unwrap_or(&serde_json::Value::Null);
            if role == "system" || role == "developer" {
                system.push(serde_json::json!({"type": "text", "text": txt_of(content)}));
                continue;
            }
            let role = if role == "assistant" { "assistant" } else { "user" };
            // tool result messages
            if role == "user" {
                if let Some(tid) = m.get("tool_call_id").and_then(|t| t.as_str()) {
                    messages.push(serde_json::json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid, "content": txt_of(content)}]}));
                    continue;
                }
            }
            match content {
                serde_json::Value::String(s) => messages.push(serde_json::json!({"role": role, "content": s})),
                serde_json::Value::Array(parts) => {
                    let mut blocks = vec![];
                    for p in parts {
                        let t = p.get("type").and_then(|x| x.as_str()).unwrap_or("text");
                        match t {
                            "text" => blocks.push(serde_json::json!({"type": "text", "text": p.get("text").and_then(|x| x.as_str()).unwrap_or("")})),
                            "image_url" => {
                                let url = p.get("image_url").and_then(|u| u.get("url")).and_then(|u| u.as_str()).unwrap_or("");
                                if url.starts_with("data:") {
                                    let (mime, data) = parse_data_url(url).ok_or("bad image data url")?;
                                    blocks.push(serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}}));
                                } else if url.is_empty() {
                                    continue;
                                } else {
                                    blocks.push(serde_json::json!({"type": "image", "source": {"type": "url", "url": url}}));
                                }
                            }
                            "input_audio" | "audio_url" => return Err("anthropic upstream does not accept audio input".into()),
                            "file" => {
                                let f = &p["file"];
                                let data = f.get("file_data").and_then(|x| x.as_str()).unwrap_or("");
                                let name = f.get("filename").and_then(|x| x.as_str()).unwrap_or("file.pdf");
                                blocks.push(serde_json::json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": data}, "title": name}));
                            }
                            _ => {}
                        }
                    }
                    // assistant tool_calls -> tool_use blocks
                    if role == "assistant" {
                        if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                            for tc in tcs {
                                let f = &tc["function"];
                                let args: serde_json::Value = f.get("arguments").and_then(|a| a.as_str()).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(serde_json::json!({}));
                                blocks.push(serde_json::json!({"type": "tool_use", "id": tc.get("id").and_then(|x| x.as_str()).unwrap_or(""), "name": f.get("name").and_then(|x| x.as_str()).unwrap_or(""), "input": args}));
                            }
                        }
                    }
                    messages.push(serde_json::json!({"role": role, "content": blocks}));
                }
                _ => {}
            }
            // assistant tool_calls with string content
            if role == "assistant" {
                if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                    if content.is_string() {
                        let mut blocks = vec![];
                        let s = content.as_str().unwrap_or("");
                        if !s.is_empty() {
                            blocks.push(serde_json::json!({"type": "text", "text": s}));
                        }
                        for tc in tcs {
                            let f = &tc["function"];
                            let args: serde_json::Value = f.get("arguments").and_then(|a| a.as_str()).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(serde_json::json!({}));
                            blocks.push(serde_json::json!({"type": "tool_use", "id": tc.get("id").and_then(|x| x.as_str()).unwrap_or(""), "name": f.get("name").and_then(|x| x.as_str()).unwrap_or(""), "input": args}));
                        }
                        if let Some(last) = messages.last_mut() {
                            *last = serde_json::json!({"role": "assistant", "content": blocks});
                        }
                    }
                }
            }
        }
    }
    let mut out = serde_json::json!({"model": model, "messages": messages});
    if !system.is_empty() {
        out["system"] = serde_json::Value::Array(system);
    }
    let max_t = body.get("max_tokens").and_then(|x| x.as_u64()).or(body.get("max_completion_tokens").and_then(|x| x.as_u64())).unwrap_or(4096);
    out["max_tokens"] = serde_json::json!(max_t);
    for k in ["temperature", "top_p", "stream", "stop_sequences"] {
        if let Some(v) = body.get(k) {
            out[k] = v.clone();
        }
    }
    if let Some(stop) = body.get("stop") {
        out["stop_sequences"] = match stop {
            serde_json::Value::String(s) => serde_json::json!([s]),
            _ => stop.clone(),
        };
    }
    if let Some(tools) = body.get("tools") {
        if let Some(arr) = tools.as_array() {
            let mut ts = vec![];
            for t in arr {
                if t.get("type").and_then(|x| x.as_str()) == Some("function") {
                    let f = &t["function"];
                    ts.push(serde_json::json!({"name": f.get("name"), "description": f.get("description"), "input_schema": f.get("parameters").unwrap_or(&serde_json::json!({"type":"object"}))}));
                }
            }
            out["tools"] = serde_json::Value::Array(ts);
        }
        if let Some(tc) = body.get("tool_choice") {
            out["tool_choice"] = match tc {
                serde_json::Value::String(s) if s == "auto" => serde_json::json!({"type": "auto"}),
                serde_json::Value::String(s) if s == "none" => serde_json::json!({"type": "none"}),
                serde_json::Value::String(s) if s == "required" => serde_json::json!({"type": "any"}),
                v if v.get("type").and_then(|x| x.as_str()) == Some("function") => serde_json::json!({"type": "tool", "name": v["function"]["name"]}),
                _ => serde_json::json!({"type": "auto"}),
            };
        }
    }
    Ok(out)
}

fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let i = rest.find(',')?;
    let (meta, data) = rest.split_at(i);
    let mime = meta.split(';').next().unwrap_or("image/jpeg").to_string();
    Some((mime, data[1..].to_string()))
}

pub fn anthropic_finish(reason: &str) -> &'static str {
    match reason {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "refusal" => "content_filter",
        _ => "stop",
    }
}

pub fn from_anthropic(resp: &serde_json::Value, req_model: &str) -> serde_json::Value {
    let id = resp.get("id").and_then(|x| x.as_str()).unwrap_or("chatcmpl-anthropic");
    let model = resp.get("model").and_then(|x| x.as_str()).unwrap_or(req_model);
    let mut text = String::new();
    let mut tool_calls = vec![];
    if let Some(blocks) = resp.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            match b.get("type").and_then(|x| x.as_str()) {
                Some("text") => text.push_str(b.get("text").and_then(|x| x.as_str()).unwrap_or("")),
                Some("tool_use") => tool_calls.push(serde_json::json!({
                    "id": b.get("id"), "type": "function",
                    "function": {"name": b.get("name"), "arguments": serde_json::to_string(b.get("input").unwrap_or(&serde_json::json!({}))).unwrap_or("{}".into())}
                })),
                _ => {}
            }
        }
    }
    let finish = anthropic_finish(resp.get("stop_reason").and_then(|x| x.as_str()).unwrap_or("end_turn"));
    let usage = resp.get("usage");
    let pt = usage.and_then(|u| u.get("input_tokens")).and_then(|x| x.as_u64()).unwrap_or(0);
    let ct = usage.and_then(|u| u.get("output_tokens")).and_then(|x| x.as_u64()).unwrap_or(0);
    let mut msg = serde_json::json!({"role": "assistant", "content": text});
    if !tool_calls.is_empty() {
        msg["tool_calls"] = serde_json::Value::Array(tool_calls);
    }
    serde_json::json!({
        "id": id, "object": "chat.completion", "created": crate::state::now_ms() / 1000,
        "model": model,
        "choices": [{"index": 0, "message": msg, "finish_reason": finish}],
        "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct}
    })
}

// ---------------------------------------------------------------- google

fn to_google(body: &serde_json::Value, model: &str, base: &str, stream: bool) -> Result<(String, serde_json::Value), String> {
    let mut contents = vec![];
    let mut system_text = String::new();
    if let Some(ms) = body.get("messages").and_then(|m| m.as_array()) {
        for m in ms {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = m.get("content").unwrap_or(&serde_json::Value::Null);
            if role == "system" || role == "developer" {
                system_text.push_str(&txt_of(content));
                system_text.push('\n');
                continue;
            }
            if m.get("tool_call_id").and_then(|t| t.as_str()).is_some() {
                // tool result -> functionResponse (need name; approximate)
                contents.push(serde_json::json!({"role": "user", "parts": [{"text": txt_of(content)}]}));
                continue;
            }
            let grole = if role == "assistant" { "model" } else { "user" };
            let mut parts = vec![];
            match content {
                serde_json::Value::String(s) => parts.push(serde_json::json!({"text": s})),
                serde_json::Value::Array(ps) => {
                    for p in ps {
                        let t = p.get("type").and_then(|x| x.as_str()).unwrap_or("text");
                        match t {
                            "text" => parts.push(serde_json::json!({"text": p.get("text").and_then(|x| x.as_str()).unwrap_or("")})),
                            "image_url" => {
                                let url = p.get("image_url").and_then(|u| u.get("url")).and_then(|u| u.as_str()).unwrap_or("");
                                if url.starts_with("data:") {
                                    if let Some((mime, data)) = parse_data_url(url) {
                                        parts.push(serde_json::json!({"inlineData": {"mimeType": mime, "data": data}}));
                                    }
                                } else {
                                    return Err("google upstream needs base64 image data (http urls unsupported)".into());
                                }
                            }
                            "input_audio" => {
                                let a = &p["input_audio"];
                                let data = a.get("data").and_then(|x| x.as_str()).unwrap_or("");
                                let fmt = a.get("format").and_then(|x| x.as_str()).unwrap_or("wav");
                                let mime = if fmt == "mp3" { "audio/mp3" } else { "audio/wav" };
                                parts.push(serde_json::json!({"inlineData": {"mimeType": mime, "data": data}}));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            if role == "assistant" {
                if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tcs {
                        let f = &tc["function"];
                        let args: serde_json::Value = f.get("arguments").and_then(|a| a.as_str()).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(serde_json::json!({}));
                        parts.push(serde_json::json!({"functionCall": {"name": f.get("name"), "args": args}}));
                    }
                }
            }
            contents.push(serde_json::json!({"role": grole, "parts": parts}));
        }
    }
    let mut out = serde_json::json!({"contents": contents});
    if !system_text.trim().is_empty() {
        out["systemInstruction"] = serde_json::json!({"parts": [{"text": system_text.trim()}]});
    }
    let mut gen = serde_json::Map::new();
    for (sk, gk) in [("temperature", "temperature"), ("top_p", "topP"), ("max_tokens", "maxOutputTokens"), ("max_completion_tokens", "maxOutputTokens")] {
        if let Some(v) = body.get(sk) {
            gen.insert(gk.to_string(), v.clone());
        }
    }
    if let Some(stop) = body.get("stop") {
        gen.insert("stopSequences".to_string(), match stop {
            serde_json::Value::String(s) => serde_json::json!([s]),
            _ => stop.clone(),
        });
    }
    if let Some(rf) = body.get("response_format") {
        let t = rf.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if t == "json_object" || t == "json_schema" {
            gen.insert("responseMimeType".to_string(), serde_json::json!("application/json"));
            if let Some(schema) = rf.get("json_schema").and_then(|s| s.get("schema")) {
                gen.insert("responseSchema".to_string(), schema.clone());
            }
        }
    }
    if !gen.is_empty() {
        out["generationConfig"] = serde_json::Value::Object(gen);
    }
    if let Some(tools) = body.get("tools") {
        if let Some(arr) = tools.as_array() {
            let mut decls = vec![];
            for t in arr {
                if t.get("type").and_then(|x| x.as_str()) == Some("function") {
                    let f = &t["function"];
                    let mut d = serde_json::json!({"name": f.get("name"), "description": f.get("description"), "parameters": f.get("parameters").unwrap_or(&serde_json::json!({"type":"object"}))});
                    // google schema dislikes additionalProperties/strict
                    strip_json_schema(&mut d);
                    decls.push(d);
                }
            }
            if !decls.is_empty() {
                out["tools"] = serde_json::json!([{"functionDeclarations": decls}]);
            }
        }
    }
    let verb = if stream { "streamGenerateContent?alt=sse" } else { "generateContent" };
    Ok((format!("{}/models/{}:{}" , base.trim_end_matches('/'), model, verb), out))
}

fn strip_json_schema(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(o) => {
            o.remove("additionalProperties");
            o.remove("strict");
            o.remove("$schema");
            for v in o.values_mut() {
                strip_json_schema(v);
            }
        }
        serde_json::Value::Array(a) => {
            for v in a {
                strip_json_schema(v);
            }
        }
        _ => {}
    }
}

fn google_finish(r: &str) -> &'static str {
    match r {
        "MAX_TOKENS" => "length",
        "SAFETY" | "BLOCKLIST" | "PROHIBITED_CONTENT" => "content_filter",
        "RECITATION" => "content_filter",
        _ => "stop",
    }
}

pub fn from_google(resp: &serde_json::Value, req_model: &str) -> serde_json::Value {
    let cand = resp.get("candidates").and_then(|c| c.as_array()).and_then(|a| a.first()).cloned().unwrap_or(serde_json::Value::Null);
    let mut text = String::new();
    let mut tool_calls = vec![];
    if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
        for p in parts {
            if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                text.push_str(t);
            }
            if let Some(fc) = p.get("functionCall") {
                tool_calls.push(serde_json::json!({
                    "id": format!("call_{}", &uuid::Uuid::new_v4().to_string()[..8]),
                    "type": "function",
                    "function": {"name": fc.get("name"), "arguments": serde_json::to_string(fc.get("args").unwrap_or(&serde_json::json!({}))).unwrap_or("{}".into())}
                }));
            }
        }
    }
    let finish = google_finish(cand.get("finishReason").and_then(|x| x.as_str()).unwrap_or("STOP"));
    let um = resp.get("usageMetadata");
    let pt = um.and_then(|u| u.get("promptTokenCount")).and_then(|x| x.as_u64()).unwrap_or(0);
    let ct = um.and_then(|u| u.get("candidatesTokenCount")).and_then(|x| x.as_u64()).unwrap_or(0);
    let mut msg = serde_json::json!({"role": "assistant", "content": text});
    if !tool_calls.is_empty() {
        msg["tool_calls"] = serde_json::Value::Array(tool_calls);
    }
    serde_json::json!({
        "id": format!("chatcmpl-g{}", &uuid::Uuid::new_v4().to_string()[..8]),
        "object": "chat.completion", "created": crate::state::now_ms() / 1000,
        "model": req_model,
        "choices": [{"index": 0, "message": msg, "finish_reason": finish}],
        "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct}
    })
}

// ---------------------------------------------------------------- SSE translate

struct ToolAcc {
    id: String,
    name: String,
    json: String,
}

pub struct StreamXlat {
    pub kind: TranslatedKind,
    pub req_model: String,
    pub resp_id: String,
    buf: Vec<u8>,
    cur_event: String,
    role_sent: bool,
    tools: HashMap<i64, ToolAcc>,
    usage_in: u64,
    usage_out: u64,
    stop: String,
    done: bool,
}

impl StreamXlat {
    pub fn new(kind: TranslatedKind, req_model: String) -> Self {
        Self {
            kind,
            req_model,
            resp_id: format!("chatcmpl-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            buf: vec![],
            cur_event: String::new(),
            role_sent: false,
            tools: HashMap::new(),
            usage_in: 0,
            usage_out: 0,
            stop: "stop".to_string(),
            done: false,
        }
    }

    fn chunk(&self, delta: serde_json::Value, finish: Option<&str>) -> Bytes {
        let c = serde_json::json!({
            "id": self.resp_id, "object": "chat.completion.chunk", "created": crate::state::now_ms()/1000,
            "model": self.req_model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]
        });
        Bytes::from(format!("data: {}\n\n", serde_json::to_string(&c).unwrap()))
    }

    fn role_chunk(&mut self) -> Option<Bytes> {
        if self.role_sent {
            return None;
        }
        self.role_sent = true;
        Some(self.chunk(serde_json::json!({"role": "assistant"}), None))
    }

    /// feed raw upstream bytes; returns translated SSE bytes to forward
    pub fn feed(&mut self, data: &[u8]) -> Vec<Bytes> {
        // strip CR so both \n\n and \r\n\r\n framing split correctly
        self.buf.extend(data.iter().filter(|b| **b != b'\r'));
        let mut out = vec![];
        loop {
            let pos = match find_double_nl(&self.buf) {
                Some(p) => p,
                None => break,
            };
            let frame: Vec<u8> = self.buf.drain(..pos).collect();
            // strip trailing newlines
            let text = String::from_utf8_lossy(&frame).to_string();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(ev) = line.strip_prefix("event:") {
                    self.cur_event = ev.trim().to_string();
                } else if let Some(d) = line.strip_prefix("data:") {
                    let d = d.trim();
                    if d == "[DONE]" {
                        out.push(Bytes::from_static(b"data: [DONE]\n\n"));
                        self.done = true;
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(d) {
                        self.handle_data(&v, &mut out);
                    }
                    self.cur_event.clear();
                }
            }
        }
        out
    }

    fn handle_data(&mut self, v: &serde_json::Value, out: &mut Vec<Bytes>) {
        match self.kind {
            TranslatedKind::Anthropic => self.handle_anthropic(v, out),
            TranslatedKind::Google => self.handle_google(v, out),
            TranslatedKind::None => {}
        }
    }

    fn handle_anthropic(&mut self, v: &serde_json::Value, out: &mut Vec<Bytes>) {
        let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match t {
            "message_start" => {
                if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                    self.usage_in = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                }
                if let Some(r) = self.role_chunk() {
                    out.push(r);
                }
            }
            "content_block_delta" => {
                let idx = v.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                let d = &v["delta"];
                let dt = d.get("type").and_then(|x| x.as_str()).unwrap_or("");
                if dt == "text_delta" {
                    if let Some(r) = self.role_chunk() {
                        out.push(r);
                    }
                    if let Some(tx) = d.get("text").and_then(|x| x.as_str()) {
                        out.push(self.chunk(serde_json::json!({"content": tx}), None));
                    }
                } else if dt == "input_json_delta" {
                    let e = self.tools.entry(idx).or_insert(ToolAcc { id: String::new(), name: String::new(), json: String::new() });
                    if let Some(pj) = d.get("partial_json").and_then(|x| x.as_str()) {
                        e.json.push_str(pj);
                    }
                }
            }
            "content_block_start" => {
                let idx = v.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                if let Some(b) = v.get("content_block") {
                    if b.get("type").and_then(|x| x.as_str()) == Some("tool_use") {
                        self.tools.insert(idx, ToolAcc {
                            id: b.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                            name: b.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                            json: String::new(),
                        });
                    }
                }
            }
            "content_block_stop" => {
                let idx = v.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                if let Some(a) = self.tools.remove(&idx) {
                    if let Some(r) = self.role_chunk() {
                        out.push(r);
                    }
                    out.push(self.chunk(serde_json::json!({"tool_calls": [{"id": a.id, "type": "function", "function": {"name": a.name, "arguments": a.json}}]}), None));
                }
            }
            "message_delta" => {
                if let Some(sr) = v.get("delta").and_then(|d| d.get("stop_reason")).and_then(|x| x.as_str()) {
                    self.stop = anthropic_finish(sr).to_string();
                }
                if let Some(u) = v.get("usage") {
                    self.usage_out = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(self.usage_out);
                }
            }
            "message_stop" => {
                let total = self.usage_in + self.usage_out;
                let c = serde_json::json!({
                    "id": self.resp_id, "object": "chat.completion.chunk", "created": crate::state::now_ms()/1000,
                    "model": self.req_model,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": self.stop}],
                    "usage": {"prompt_tokens": self.usage_in, "completion_tokens": self.usage_out, "total_tokens": total}
                });
                out.push(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&c).unwrap())));
                out.push(Bytes::from_static(b"data: [DONE]\n\n"));
                self.done = true;
            }
            "error" => {
                out.push(Bytes::from(format!("data: {}\n\n", serde_json::to_string(v).unwrap_or("{}".into()))));
            }
            _ => {}
        }
    }

    fn handle_google(&mut self, v: &serde_json::Value, out: &mut Vec<Bytes>) {
        let cand = match v.get("candidates").and_then(|c| c.as_array()).and_then(|a| a.first()) {
            Some(c) => c,
            None => return,
        };
        if let Some(r) = self.role_chunk() {
            out.push(r);
        }
        if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
            for p in parts {
                if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                    if !t.is_empty() {
                        out.push(self.chunk(serde_json::json!({"content": t}), None));
                    }
                }
                if let Some(fc) = p.get("functionCall") {
                    out.push(self.chunk(serde_json::json!({"tool_calls": [{
                        "id": format!("call_{}", &uuid::Uuid::new_v4().to_string()[..8]),
                        "type": "function",
                        "function": {"name": fc.get("name"), "arguments": serde_json::to_string(fc.get("args").unwrap_or(&serde_json::json!({}))).unwrap_or("{}".into())}
                    }]}), None));
                }
            }
        }
        if let Some(um) = v.get("usageMetadata") {
            self.usage_in = um.get("promptTokenCount").and_then(|x| x.as_u64()).unwrap_or(self.usage_in);
            let c = um.get("candidatesTokenCount").and_then(|x| x.as_u64()).unwrap_or(0);
            if c > 0 {
                self.usage_out = c;
            }
        }
        if let Some(fr) = cand.get("finishReason").and_then(|x| x.as_str()) {
            if fr != "STOP" || self.usage_out > 0 {
                self.stop = google_finish(fr).to_string();
                let total = self.usage_in + self.usage_out;
                let c = serde_json::json!({
                    "id": self.resp_id, "object": "chat.completion.chunk", "created": crate::state::now_ms()/1000,
                    "model": self.req_model,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": self.stop}],
                    "usage": {"prompt_tokens": self.usage_in, "completion_tokens": self.usage_out, "total_tokens": total}
                });
                out.push(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&c).unwrap())));
            }
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Graceful terminator when upstream closes without a stop event.
    pub fn finish(&mut self) -> Vec<Bytes> {
        if self.done || !self.role_sent {
            return vec![];
        }
        self.done = true;
        let c = serde_json::json!({
            "id": self.resp_id, "object": "chat.completion.chunk", "created": crate::state::now_ms()/1000,
            "model": self.req_model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": self.stop}],
        });
        vec![
            Bytes::from(format!("data: {}\n\n", serde_json::to_string(&c).unwrap())),
            Bytes::from_static(b"data: [DONE]\n\n"),
        ]
    }
}

fn find_double_nl(buf: &[u8]) -> Option<usize> {
    if buf.len() < 2 {
        return None;
    }
    for i in 0..buf.len() - 1 {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
    }
    None
}

/// Wrap an upstream byte stream into a translated SSE stream.
pub fn translated_stream<S, E>(kind: TranslatedKind, req_model: String, upstream: S) -> impl Stream<Item = Result<Bytes, E>>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    futures_util::stream::unfold((Box::new(upstream) as Box<S>, StreamXlat::new(kind, req_model), std::collections::VecDeque::<Bytes>::new(), false), |(mut up, mut x, mut q, mut ended)| async move {
        loop {
            if let Some(b) = q.pop_front() {
                return Some((Ok(b), (up, x, q, ended)));
            }
            if ended {
                return None;
            }
            match futures_util::StreamExt::next(&mut *up).await {
                Some(Ok(chunk)) => {
                    for b in x.feed(&chunk) {
                        q.push_back(b);
                    }
                    if x.is_done() {
                        ended = true;
                    }
                }
                Some(Err(e)) => return Some((Err(e), (up, x, q, ended))),
                None => {
                    for b in x.finish() {
                        q.push_back(b);
                    }
                    ended = true;
                    if q.is_empty() {
                        return None;
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------- decisions

/// Send a Decisions request. source: "typesafe" | "openrouter".
pub async fn send_decision(
    http: &reqwest::Client,
    source: &str,
    slug: &str,
    state_val: &serde_json::Value,
    questions: &serde_json::Value,
    typesafe_cfg: Option<&ProviderConfig>,
    openrouter_cfg: Option<&ProviderConfig>,
    provider_prefs: Option<&serde_json::Value>,
    timeout_ms: u64,
) -> Result<serde_json::Value, (u16, String)> {
    let map = |e: reqwest::Error| {
        if e.is_timeout() {
            (504, "decision timeout".to_string())
        } else if e.is_connect() {
            (502, format!("decision connect error: {e}"))
        } else {
            (502, format!("decision transport error: {e}"))
        }
    };
    if source == "typesafe" {
        let cfg = typesafe_cfg.ok_or((400, "typesafe provider not linked".to_string()))?;
        if cfg.api_key.is_empty() {
            return Err((400, "typesafe api_key missing".to_string()));
        }
        let base = base_url("typesafe", cfg).map_err(|e| (400, e))?;
        let model = slug.strip_prefix("typesafe/").unwrap_or(slug);
        let body = serde_json::json!({"model": model, "state": state_val, "questions": questions});
        let resp = http
            .post(format!("{}/v1/systemone", base.trim_end_matches('/')))
            .bearer_auth(cfg.api_key.clone())
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .json(&body)
            .send()
            .await
            .map_err(map)?;
        let st = resp.status().as_u16();
        let v: serde_json::Value = resp.json().await.map_err(|e| (502, format!("bad decision response: {e}")))?;
        if !(200..300).contains(&st) {
            return Err((st, v.to_string()));
        }
        let mut out = v;
        if out.get("id").is_none() {
            out["id"] = serde_json::json!(format!("dec-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        }
        Ok(out)
    } else if source == "openrouter" {
        let cfg = openrouter_cfg.ok_or((400, "openrouter provider not linked".to_string()))?;
        if cfg.api_key.is_empty() {
            return Err((400, "openrouter api_key missing".to_string()));
        }
        let mut body = serde_json::json!({"model": slug, "state": state_val, "questions": questions});
        if let Some(p) = provider_prefs {
            body["provider"] = p.clone();
        }
        let mut r = http
            .post("https://openrouter.ai/api/alpha/decisions")
            .bearer_auth(cfg.api_key.clone())
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .json(&body);
        r = apply_headers(r, &cfg.extra_headers);
        let resp = r.send().await.map_err(map)?;
        let st = resp.status().as_u16();
        let v: serde_json::Value = resp.json().await.map_err(|e| (502, format!("bad decision response: {e}")))?;
        if !(200..300).contains(&st) {
            return Err((st, v.to_string()));
        }
        Ok(v)
    } else {
        Err((400, format!("unknown decision source '{source}'")))
    }
}
