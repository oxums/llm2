# llm2

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

Success responses carry `X-llm2-model: provider/model` with the model that
actually served the request (plus `X-Routed-By` with the same value).

## Console UI (`/ui/`)

Login with the API key, then manage providers/aliases or open the
**Playground**: pick Chat / Embeddings / Decisions, choose any model or alias,
tune messages, sampling params, and routing flags (or paste raw JSON), stream
responses live with latency + `X-llm2-model` shown, keep a run history, and
copy any run as cURL.

Per-request flags (body field **or** `x-` header — **header wins**, always
stripped before upstream):

| flag | meaning |
|---|---|
| `ignore_models: [...]` | remove from candidates |
| `prefer_models: [...]` | try first, in order |
| `any_prefer_provider: "groq"` | `any:` only — this provider first |
| `only_providers: [...]` / `only_models: [...]` | keep only matches |
| `prefer_models_ignore_cooldown` (default `true`) | preferred picks bypass the 60s fail cooldown; also covers `any_prefer_provider` |
| `disable_thinking_summaries: true` | opt out — no thought summaries |
| `thinking_level: "minimal"\|"low"\|"medium"\|"high"` | Gemini 3 thinking depth (`reasoning_effort` also accepted as alias) |
| `thinking_budget: 1024` | Gemini 2.5 token budget (exclusive with `thinking_level`) |
| `include_thoughts: false` | explicit off (the disable flag wins when both are set) |
| `thinking_config: {...}` | raw `thinkingConfig` merge (explicit knobs above win) |

Model key grammar: `provider:model`, `provider/model`, `any:model`, bare `id`
(matches every provider).

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
