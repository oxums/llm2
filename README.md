# LLM Proxy (`llm2`)

OpenAI-compatible LLM proxy in Rust (Axum): route `provider:model`, `any:model`
or named **aliases** across ~25 providers, with round-robin / failover /
classifier routing, multimodal filtering, embeddings + decision-model support,
and a dark console UI at `/ui/`.

## Quick start

```bash
cp config.example.json config.json   # config.json is gitignored
export API_KEY=admin                 # default if unset
cargo run --release
# UI: http://localhost:3000/ui/   Proxy: http://localhost:3000/v1
```

Env: `HOST` (default `0.0.0.0`), `PORT` (3000), `API_KEY` (admin),
`CONFIG_PATH` (config.json), `MODELS_DEV_URL`, `RUST_LOG`.

Docker:

```bash
docker build -t llm-proxy .
docker run -p 3000:3000 -e API_KEY=secret -v ./config.json:/app/config.json llm-proxy
```

## Calling the proxy

Same `Authorization: Bearer $API_KEY` for `/v1/*` and `/api/*`. The UI login
stores the key in `localStorage`.

```bash
# direct: [provider]:[model] or [provider]/[model]
curl $BASE/v1/chat/completions -H "Authorization: Bearer $API_KEY" \
  -d '{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}'

# any provider serving the model (first healthy wins)
curl $BASE/v1/chat/completions ... -d '{"model":"any:gpt-4o-mini",...}'

# alias (routing group)
curl $BASE/v1/chat/completions ... -d '{"model":"fast",...}'

# streaming: add "stream": true (SSE, translated for Anthropic/Google too)
# embeddings
curl $BASE/v1/embeddings ... -d '{"model":"openai/text-embedding-3-small","input":"hi"}'
# decisions (TypeSafe System One / OpenRouter alpha shape)
curl $BASE/v1/decisions ... -d '{"model":"typesafe/jev-latest","state":"...","questions":{"q":{"type":"choice","instructions":"...","criteria":{"a":"...","b":"..."}}}}'
```

Per-request flags (body field **or** `x-` header — **header wins**, always
stripped before upstream):

| flag | meaning |
|---|---|
| `ignore_models: [...]` | remove from candidates |
| `prefer_models: [...]` | try first, in order |
| `any_prefer_provider: "groq"` | `any:` only — this provider first |
| `only_providers: [...]` / `only_models: [...]` | keep only matches |
| `prefer_models_ignore_cooldown` (default `true`) | preferred picks bypass the 60s fail cooldown; also covers `any_prefer_provider` |

Model key grammar: `provider:model`, `provider/model`, `any:model`, bare `id`
(matches every provider).

## Aliases

- **chat**: `round_robin` · `failover` (try in order, 60s cooldown on
  429/5xx, immediate retry if all cooling) · `custom_order` · `classifier`.
- **embedding** / **decision**: failover-family only (no classifier).
- Members must be uniform kind (all chat / all embedding / all decision).
- Multimodal gate: image/audio/video/pdf requests only reach capable models
  (catalog + per-member overrides); spoofed `/v1/models` advertises the union.
- Per-member: `extra_headers`, `extra_body` (e.g. `{"service_tier":"fast"}`),
  capability overrides, `option_description` (classifier criteria text).
- **Classifier**: backend `jev` (TypeSafe-direct `typesafe/*` or
  OpenRouter-decisions `openrouter/*`: Choice question over members,
  `min_confidence` gate → `fallback`) or `text_structured` (chat judge with
  JSON output). Extra latency accepted by design.

## Providers

Passthrough (OpenAI-compatible): openai, groq, togetherai, fireworks,
deepinfra, cerebras, deepseek, mistral, cohere, xai, perplexity, openrouter,
ollama, lmstudio, vercel, azure-openai, generic `openai-compatible`.
Translated: **anthropic**, **google** (non-stream + SSE, text/image/tools).
Unsupported in v1: `amazon-bedrock` (SigV4), `google-vertex` (OAuth),
`elevenlabs` (audio-only) — clear 501-style errors.
Classifier-only: `typesafe` (System One) + decision slugs via `openrouter`
(`typesafe/*`, `cloudflare/clef[-flash]`, `liquid/d1`, `respan/span-01[-lite]`,
`jaredpalmer/kev-4b`, `openai/gpt-6-luna-decisions`, `perplexity/pplx-decider-v1-27b`;
models.dev omits these, fetched from OpenRouter when linked).

Model catalog: `https://models.dev/api.json` at startup + 12h refresh
(+ bundled empty fallback offline).

## Notes for 3k req/s

Shared `reqwest` pool (256 idle/host, keepalive), lock-free cooldown/RR maps
(`DashMap`), `RwLock` config (hot path = reads), zero-copy SSE passthrough,
10MB body cap, 4096 concurrency limit, per-request atomics for metrics
(`/api/stats`, `/healthz`).

## Layout

`src/{main,config,state,catalog,providers,router,classifier,proxy,admin,ui}.rs`
· `ui/{index.html,app.js}` (Tailwind PlayCDN, embedded) · `config.example.json`
