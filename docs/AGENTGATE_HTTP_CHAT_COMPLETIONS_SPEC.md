# AgentGate HTTP `/chat/completions` + AI Gateway Spec

**Status:** Draft v2 (detailed, foundation-mapped)  
**Date:** 2026-11-27  
**Owner:** OpenCode (lead)  
**Scope:** Evolve AgentGate from CLI-bridge-first gateway into a hybrid AI gateway that supports direct HTTP OpenAI-compatible upstreams, provider/model/admin management, quota accounting, API key lifecycle, proxy pools, and free-model load balancing, while preserving the existing CLI bridge path.

---

## 1. Executive Summary

AgentGate today serves OpenAI-compatible `/v1/chat/completions` and routes requests through configured CLI/bridge providers (`opencode`, `gemini`, `ollama` CLI mode, etc.). This works for agent/tooling flows but is expensive for plain chat completions and fragile under process pressure.

This spec defines:

1. **HTTP upstream mode** for OpenAI-compatible `/chat/completions`.
2. **Provider mappings** for requested providers, with endpoint mappings only where verified.
3. **Gateway backend** for provider/model/key/quota/proxy management.
4. **Quota accounting** for content in/out (tokens/bytes/requests) and key usage limits.
5. **API key generation lifecycle** (create, expiry, rotation, revoke, usage counters).
6. **Proxy single/pool** support for upstream HTTP providers.
7. **Free-forever load balancing/routing** across available free models with quota-aware fallback to `9router/free-forever`.

> **Deployment rule:** do not deploy changes until explicitly requested by user.

---

## 2. Current Foundation (as-is)

### 2.1 HTTP API (Axum)
Source: `src/api.rs`

Existing endpoints:
- `GET /` - root info
- `GET /v1/models` - model list
- `POST /v1/chat/completions` - chat completions (stream/non-stream)
- `GET /health` - health
- `GET /_metrics` - Prometheus metrics
- `POST/DELETE /_bridge/mcp/:session_id` - bridge MCP
- `POST /_bridge/resolve/:call_id` - tool-call resolve
- `GET /_bridge/preflight/:session_id` - isolation preflight
- `POST /admin/provider` - **stub** (logs, returns ok)
- `POST /admin/key` - **stub**
- `POST /admin/proxy` - **stub**
- `POST /admin/skill` - stub
- `POST /admin/mcp` - stub

Auth today:
- `ServerConfig.api_keys: Vec<String>` static config keys (`src/config.rs`).
- Bridge endpoints use per-session bearer token (`BridgeServer::validate_token`).
- No key metadata, no expiry, no generation timestamps, no per-key quota, no usage accounting.

### 2.2 Config Model
Source: `src/config.rs`

Current relevant structures:
- `[server] host/port/api_keys/request_timeout_secs/cors`
- `[defaults] workdir/max_concurrency_per_provider/model_cache_ttl_secs/first_token_timeout_secs`
- `[routing] no_tools_policy/on_client_tools/default_workspace/model_aliases`
- `[bridge] ... reroute_free_models, passthrough_host_mcp, allow_client_mcp, isolation, limits`
- `[limits] max_inject/max_tools/max_prompt_chars/max_result_bytes`
- `[[provider]]` - CLI-oriented fields: `id, enabled, bin, models, models_cmd, chat_cmd, strip_ansi, base_url, num_ctx, bridge, transport, model_no_tool_support, reasoning*, keep_alive, ollama_* , prompt_tools_fallback`

Gap: no first-class HTTP upstream provider type, no upstream auth config, no proxy pool, no quota policy, no key lifecycle store.

### 2.3 Provider Runtime
Source: `src/provider.rs`, `src/provider/bridge/*`, `src/service.rs`

- `CliProviderImpl` executes CLI commands for chat completions.
- `BridgeCliProvider` executes bridge sessions for tool-capable flows.
- `ServiceRegistry` maintains bridge/native providers, model cache, quota-disabled model state, quota-aware reroute, failover to `opencode/9router/free-forever`.
- Recent fixes include quota disabled state persisted to `quota_state.json`, watcher re-enable at next UTC day, force-fresh CLI session on model reroute/failover.

Gap: no `HttpProvider` implementation that forwards to upstream `/v1/chat/completions`.

### 2.4 Quota / Routing
Source: `src/service.rs`

Current behavior:
- Detects quota/free-limit/429 patterns.
- Marks opencode model quota-disabled until next UTC day (persisted).
- Reroutes free/disabled opencode model to other configured free models, fallback `opencode/9router/free-forever`.
- Stream failover only when no meaningful output emitted.

Gap: quota is model-disable state, not token/content accounting; no per-key/per-provider budget; no load balancer across multiple free upstreams/endpoints.

### 2.5 Admin / Data Plane Gaps

Missing backend capabilities requested:
- quota limits for content in/out
- API key generation times / lifecycle
- add model
- add provider
- add proxy single / proxy pool
- AI-gateway style management

---

## 3. Goals

- Add HTTP upstream mode while keeping CLI bridge mode intact.
- Keep external AgentGate API OpenAI-compatible (`/v1/chat/completions`, `/v1/models`).
- Support requested providers via HTTP with **verified endpoint mappings**; no guessed endpoints.
- Add gateway management plane: providers, models, keys, quotas, proxies, routing policies.
- Add usage accounting: requests, input/output tokens (and bytes fallback), errors, quota consumption.
- Add API key generation with timestamps, expiry, max uses, rate/quota limits, revocation.
- Add proxy single and proxy pool with health/failover.
- Add free-forever load balancing with quota-aware model disable and fallback.

## 4. Non-Goals (Phase 1)

- Full billing/invoicing system.
- Replacing CLI bridge for tool-heavy local MCP workflows.
- Implementing every provider-native API; first-class target is OpenAI-compatible chat completions.
- Executing client-advertised stdio MCP on host by default in HTTP mode.
- Guessing undocumented provider endpoints.

---

## 5. Target Architecture

```text
Client (AionUI/OpenAI SDK)
        |
        v
AgentGate /v1/chat/completions
        |
        +-- AuthN/AuthZ (API key lifecycle + quota)
        +-- Request normalization + policy
        +-- Router (model alias, mode, free-pool, quota, LB)
        |      |\
        |      | \-- CLI Bridge Provider (existing)
        |      \---- HTTP Upstream Provider (new)
        |                 |
        |                 +-- Proxy selector (single/pool)
        |                 +-- Upstream adapter (OpenAI-compatible / provider-specific compat)
        |                 \-- SSE/JSON normalizer
        +-- Usage recorder (in/out tokens, bytes, cost estimate, status)
        \-- Metrics/logs/audit
```

### 5.1 Execution Modes

| Mode | Use | Behavior |
|---|---|---|
| `cli` | Tool/MCP/local-agent workflows | Existing bridge/CLI spawn |
| `http` | Plain chat/completions, scalable upstreams | Direct HTTP to upstream, no CLI spawn |
| `auto` | Router decides | Prefer `http` for chat-only; `cli` when tool/runtime requires bridge |

---

## 6. Provider Endpoint Mapping (verified / needs confirmation)

> **Rule:** Only verified mappings are marked `Confirmed`. Anything unverified is `Needs Confirmation` and must not be hardcoded as default without user confirmation.

### 6.1 Confirmed OpenAI-compatible mappings

| Provider (requested) | Status | Provider ID(s) | Base URL | Chat Completions Endpoint | Auth | Compatibility | Source |
|---|---|---|---|---|---|---|---|
| OpenRouter | **Confirmed** | `openrouter` | `https://openrouter.ai/api/v1` | `POST /chat/completions` | `Authorization: Bearer <OPENROUTER_API_KEY>` | OpenAI-compatible | https://openrouter.ai/docs/api-reference/chat-completion |
| Groq | **Confirmed** | `groq` | `https://api.groq.com/openai/v1` | `POST /chat/completions` | `Authorization: Bearer <GROQ_API_KEY>` | OpenAI-compatible | https://console.groq.com/docs/openai |
| Gemini (Google AI Studio, OpenAI compat) | **Confirmed** | `gemini-http` (separate from existing CLI `gemini`) | `https://generativelanguage.googleapis.com/v1beta/openai` | `POST /chat/completions` | `Authorization: Bearer <GEMINI_API_KEY>` | OpenAI-compatible (Gemini subset) | https://ai.google.dev/gemini-api/docs/openai |
| Kilo / Kilocode Gateway | **Confirmed** (user confirmed both aliases) | `kilo`, `kilocode` (alias) | `https://ai-gateway.kilo.ai/api/v1` | `POST /chat/completions` | `Authorization: Bearer <KILO_API_KEY>` | OpenAI-compatible | https://kilo.ai/docs/gateway , https://kilo.ai/docs/gateway/api-reference |
| Agnes AI | **Confirmed** (user provided + official docs) | `agnes-ai` | `https://apihub.agnes-ai.com/v1` | `POST /chat/completions` | `Authorization: Bearer <AGNES_API_KEY>` | OpenAI-compatible | https://www.agnes-ai.com/en/docs/overview |
| Nvidia NIM | **Confirmed** | `nvidia` | `https://integrate.api.nvidia.com/v1` | `POST /chat/completions` | `Authorization: Bearer <NVIDIA_API_KEY>` (`nvapi-*`) | OpenAI-compatible NIM | https://docs.api.nvidia.com/nim/reference/llm-apis |

Confirmed detail notes:
- **Kilo:** official API reference verifies `GET /models` (no auth), SSE streaming,
  `tool_choice`/`response_format`/`seed`, and 429 rate-limit / 402 balance /
  502-503 upstream errors. Both `kilo` and `kilocode` resolve to the same mapping.
- **Agnes AI:** official docs verify the base URL, Bearer auth, `POST /v1/chat/completions`,
  SSE streaming and tool calls. Model IDs are `agnes-3.0-flash` (preferred) and
  `agnes-2.5-flash` / `agnes-2.5-pro`; note `agnes-2.5-flash` retires 2026-11-30.
- **Nvidia NIM:** see `docs/PROVIDER_NVIDIA_NIM.md` for verified SSE/tool-call/usage
  behavior and the `additionalProperties = false` 422 caveat.

### 6.2 Confirmed split / still needs implementation detail

| Provider (requested) | Status | Decision | Remaining detail |
|---|---|---|---|
| OpenCode | **Confirmed split: CLI + HTTP separate** | Keep existing `opencode` CLI/bridge provider; add separate HTTP provider id `opencode-http`. **No default HTTP URL is hardcoded** and no auto-switch from CLI to HTTP. `opencode-http` stays unverified/disabled until the operator selects an explicit `base_url`. | Which HTTP target to use: OpenCode Go (`https://opencode.ai/inference/go/openai/v1`), pay-as-you-go (`https://opencode.ai/inference/openai/v1`), or local `opencode serve` endpoint. Session header `x-opencode-session` configurable. |

### 6.3 Mapping rules

- Store provider mapping as config/data, not hardcoded in router logic.
- Each provider has `adapter` (default `openai_compatible_chat`).
- Endpoint path is configurable; default `/chat/completions`.
- For Gemini OpenAI-compat, use the confirmed base URL above; do not use native `:generateContent` in this adapter unless a separate `gemini_native` adapter is added.
- For Kilo Gateway, accept `kilo` and `kilocode` as aliases to same config.
- Agnes AI uses confirmed base `https://apihub.agnes-ai.com/v1` and model IDs as configured (prefer `agnes-3.0-flash`; `agnes-2.5-flash` retires 2026-11-30).
- Nvidia NIM uses `https://integrate.api.nvidia.com/v1`; model IDs follow NIM catalog (e.g. `meta/llama-3.3-70b-instruct`).
- For Kilo Gateway, optional org/task headers may be configured but are not required by default.
- Only mappings with `mapping_status = verified` may be enabled by default; unverified entries stay disabled until confirmed.
- OpenCode CLI and HTTP must be separate provider entries/routes; do not silently switch CLI provider to HTTP.

### 6.4 Combo Model (model chain/fallback group)

User request: "tambahan untuk nvdia dan juga buatkan combo model agar bisa..."

**Definition:** A `combo` model is a virtual model ID that maps to an ordered list of candidate models/providers with routing policy. It is not an upstream model; AgentGate resolves it at request time.

```toml
[[model]]
id = "combo/free-smart"
type = "combo"
enabled = true
strategy = "fallback" # fallback|weighted|round_robin|race_first_success (phase 2)
candidates = [
  { model = "agg/nvidia/meta/llama-3.3-70b-instruct", weight = 1 },
  { model = "agg/groq/llama-3.1-8b-instant", weight = 1 },
  { model = "opencode/9router/free-forever", weight = 1 }
]
stop_on = ["success"]
fallback_on = ["quota", "rate_limit", "timeout", "5xx", "unavailable", "empty_response"]
max_attempts = 3
```

**Rules:**
- No request to disabled/quota-exhausted candidate unless probe mode explicitly enabled.
- Record each attempt in metrics/audit; client sees final response or aggregated error with attempt summary.
- Streaming: only failover before meaningful output (same rule as current quota failover).
- Combo can include CLI and HTTP targets, but mode compatibility must be checked (`cli` candidate only if bridge/tool flow allowed).
- Admin API: CRUD for combo models; validate candidate existence and cycle prevention.

**Implemented (`src/provider/combo.rs` + `src/service.rs`):** `ComboRegistry` exposes
global `create/update/delete/get/list` CRUD with structural validation (non-empty
ids/candidates, `max_attempts <= candidates.len()`, unknown fallback conditions,
cycle prevention including forward references to other combos) and mode
compatibility checks via a resolver callback. Routing calls record per-attempt
metrics (`combo_attempt`, `combo_fallback`) and only advance on configured
`fallback_on` conditions — never after meaningful stream output. Both CLI and
HTTP candidates are supported; recursion is bounded and combo streaming uses
owned provider handles so no CLI/bridge session is created in HTTP mode.
The admin HTTP routes can call `crate::provider::combo::registry()` directly.

---

## 7. Detailed Config Schema (proposed)

### 7.1 Server/auth

```toml
[server]
host = "127.0.0.1"
port = 8787
request_timeout_secs = 120
cors = false

# Legacy static keys (backward compat, read-only migration source)
api_keys = []

[server.auth]
enabled = true
key_header = "Authorization"      # Authorization: Bearer <key>
allow_x_api_key = false
admin_api_keys = []                # separate admin keys; env recommended
key_store_path = "data/api_keys.json" # or sqlite in future
```

### 7.2 Provider

```toml
[[provider]]
id = "openrouter"
enabled = true
type = "http_openai_compatible"    # new; existing CLI providers keep current behavior
mode = "http"                      # cli|http|auto
base_url = "https://openrouter.ai/api/v1"
chat_completions_path = "/chat/completions"
models_path = "/models"
auth_type = "bearer"               # bearer|header|none
api_key_env = "OPENROUTER_API_KEY"
api_key = ""                        # discouraged; env/file preferred
default_model = "openrouter/auto"
models = ["openai/gpt-4o-mini"]
timeout_secs = 120
max_retries = 2
retry_backoff_ms = [250, 1000]
forward_tools = true
forward_tool_choice = true
forward_response_format = true
forward_stream = true
stream_idle_timeout_secs = 120
verify_tls = true

[provider.quota]
enabled = true
max_requests_per_minute = 60
max_requests_per_day = 1000
max_input_tokens_per_day = 500000
max_output_tokens_per_day = 500000
max_concurrent = 20

[provider.proxy]
mode = "none"                      # none|single|pool
single_url = ""                     # e.g. http://user:pass@127.0.0.1:8080
pool = ["http://proxy-a:8080", "http://proxy-b:8080"]
strategy = "round_robin"            # round_robin|random|least_errors|sticky
healthcheck_url = "https://openrouter.ai/api/v1/models"
healthcheck_interval_secs = 60
ban_on_429_secs = 60
ban_on_5xx_secs = 30
```

### 7.3 Model catalog / routing

```toml
[[model]]
id = "agg/openrouter/gpt-4o-mini"
provider_id = "openrouter"
upstream_model = "openai/gpt-4o-mini"
enabled = true
mode = "http"
tags = ["chat", "tool", "paid"]
free = false

[[model]]
id = "agg/groq/llama-3.1-8b-instant"
provider_id = "groq"
upstream_model = "llama-3.1-8b-instant"
enabled = true
mode = "http"
tags = ["chat", "fast", "free-pool-candidate"]
free = true

[bridge.routing]
default_mode = "auto"

[[bridge.routing.rules]]
name = "free-to-pool"
match_model_regex = "(?i)(^|/)free($|-|_)"
action = "load_balance"
pool = "free_forever"
fallback = "opencode/9router/free-forever"

[load_balance.pools.free_forever]
strategy = "weighted_round_robin"   # round_robin|weighted_round_robin|least_latency|least_errors|random
sticky_key = "session_id"           # none|session_id|api_key
cooldown_on_quota_secs = 86400      # or next_utc_day policy
members = [
  { model = "agg/groq/...", weight = 3, enabled = true },
  { model = "agg/openrouter/...", weight = 1, enabled = true },
]
fallback = ["opencode/9router/free-forever"]
```

### 7.4 API key lifecycle (new)

```toml
[quota.defaults]
requests_per_minute = 60
requests_per_day = 2000
input_tokens_per_day = 1000000
output_tokens_per_day = 1000000
max_concurrent_requests = 10
```

Key record (stored in key store, not plain TOML for generated keys):

```json
{
  "id": "key_01...",
  "name": "aionui-prod",
  "prefix": "agk_xxx",
  "hash": "argon2id/bcrypt-hash",
  "created_at": "2026-11-27T00:00:00Z",
  "updated_at": "2026-11-27T00:00:00Z",
  "last_used_at": null,
  "expires_at": "2026-12-27T00:00:00Z",
  "revoked_at": null,
  "status": "active",
  "scopes": ["chat.completions", "models.read"],
  "allowed_providers": ["openrouter", "groq"],
  "allowed_models": ["agg/*"],
  "max_uses_total": 100000,
  "used_count": 0,
  "quota": {
    "requests_per_minute": 60,
    "requests_per_day": 2000,
    "input_tokens_per_day": 1000000,
    "output_tokens_per_day": 1000000
  },
  "usage": {
    "requests_total": 0,
    "input_tokens_total": 0,
    "output_tokens_total": 0,
    "content_in_bytes_total": 0,
    "content_out_bytes_total": 0
  }
}
```

---

## 8. API Design (new/extended)

### 8.1 Data plane (OpenAI-compatible)

- `POST /v1/chat/completions` - unchanged path; add router decision for `cli|http|auto`.
- `GET /v1/models` - merge CLI-discovered + HTTP catalog models.
- `GET /health` - include provider/proxy pool summary (optional detail flag).
- `GET /_metrics` - add HTTP/quota/proxy/LB metrics.

### 8.2 Admin plane (new, authenticated)

All admin endpoints require admin key/scope. Responses must redact secrets.

#### Providers
- `GET /admin/providers`
- `POST /admin/providers` - add provider
- `GET /admin/providers/:id`
- `PATCH /admin/providers/:id`
- `DELETE /admin/providers/:id` (soft-disable preferred)
- `POST /admin/providers/:id/test` - smoke test `/models` or small chat completion

#### Models / Combos
- `GET /admin/models`
- `POST /admin/models` - add model mapping or combo model
- `PATCH /admin/models/:id`
- `DELETE /admin/models/:id`
- `POST /admin/models/sync?provider_id=...` - pull upstream `/models` where supported
- `GET /admin/combos` / `POST /admin/combos` / `PATCH /admin/combos/:id` / `DELETE /admin/combos/:id` (optional alias; can be unified under models with `type=combo`)

#### API keys
- `POST /admin/keys` - generate key; returns plaintext **once** + metadata
- `GET /admin/keys` - list metadata only (no plaintext/hash)
- `GET /admin/keys/:id`
- `PATCH /admin/keys/:id` - update limits/expiry/scopes/status
- `POST /admin/keys/:id/revoke`
- `POST /admin/keys/:id/rotate` - generate new secret, revoke/overlap old

#### Quotas / usage
- `GET /admin/usage?from=&to=&provider=&model=&key_id=`
- `GET /admin/quotas`
- `PATCH /admin/quotas/defaults`
- `GET /admin/quotas/keys/:id`
- `PATCH /admin/quotas/keys/:id`

#### Proxies
- `GET /admin/proxies`
- `POST /admin/proxies` - add single/pool
- `PATCH /admin/proxies/:id`
- `DELETE /admin/proxies/:id`
- `POST /admin/proxies/:id/test`

#### Routing / load balancer
- `GET /admin/routing`
- `PUT /admin/routing` - validate + apply
- `GET /admin/pools`
- `PUT /admin/pools/:name`

### 8.3 Backward compatibility

Existing stub endpoints (`/admin/provider`, `/admin/key`, `/admin/proxy`) should be kept temporarily as aliases to new handlers with deprecation warning headers/logs.

---

## 9. Request/Response Normalization

### 9.1 Inbound (client -> AgentGate)

Accept OpenAI-style request. Preserve:
- `model`, `messages`, `stream`, `tools`, `tool_choice`, `temperature`, `top_p`, `max_tokens`, `stop`, `response_format`, `seed`, `user`.

AgentGate-internal fields stripped before upstream:
- `agentgate_*`, internal session/run fields, client-only MCP control fields not supported upstream.

### 9.2 Outbound (AgentGate -> HTTP upstream)

- Map `model` to `upstream_model`.
- Set `Authorization`/custom headers per provider config.
- Inject proxy if configured.
- Apply timeout/retry policy.
- For streaming, set `Accept: text/event-stream` and handle SSE.

### 9.3 Response (upstream -> client)

- Non-stream JSON: normalize to OpenAI `chat.completion`.
- Stream SSE: normalize to `chat.completion.chunk`, pass through tool_calls deltas safely, ensure `[DONE]`.
- Include AgentGate metadata only in internal logs/metrics, not in OpenAI response body unless explicitly enabled.

---

## 10. Quota, Accounting, and Limits

### 10.1 Accounting dimensions

Per request, record:
- `request_id`, `key_id`, `provider_id`, `model`, `upstream_model`
- `mode` (`cli|http`)
- `status`, `error_class`
- `started_at`, `ended_at`, `latency_ms`
- `input_tokens`, `output_tokens`, `total_tokens`
- `content_in_bytes`, `content_out_bytes` (raw payload/body size; always available fallback)
- `stream` boolean
- `proxy_id`/`proxy_endpoint` (redacted)

Token source priority:
1. Upstream usage fields (`usage.prompt_tokens/completion_tokens/total_tokens`).
2. Stream final usage chunk if provided.
3. Fallback estimator (chars/4 or tokenizer adapter) marked `estimated=true`.

### 10.2 Limit enforcement

Enforce at key/provider/model/pool levels:
- RPM / RPD
- input tokens/day, output tokens/day, total tokens/day
- max concurrent requests
- max request body bytes
- max uses total / max uses per day

On limit exceeded:
- `429` with clear `error.type=quota_exceeded`, include reset time and scope.
- Record metric and audit event.

### 10.3 Quota state for free models (updated requirement)

Extend current `quota_state.json` model disable behavior:
- key: provider/model (+ optional pool)
- fields: `disabled_at` (first free.limit/quota notification timestamp), `last_seen_quota_at`, `retry_after`, `reason`, `source` (429, provider.free-limit, explicit quota, admin), `probe_failures`, `recheck_count`.
- **On first quota/free-limit notification**: mark model disabled immediately, record `disabled_at` (do not overwrite on repeated notifications unless configured), persist state, route away from it. No further completion requests should be sent to that model while disabled except controlled probe.
- **Watcher interval**: default **12 hours** (configurable `quota.watcher_interval_secs`, default 43200). Watcher checks disabled models; it may:
  - re-enable after cooldown window (default: 12h from `last_seen_quota_at` or next eligible window), OR
  - perform one lightweight probe request (opt-in, `quota.probe_on_recheck=true`) before re-enabling to avoid burning quota. Default safe behavior: re-enable on schedule, disable again on first quota hit.
- **All free models quota-limited**: if no eligible free model remains in pool/provider candidates, route to `opencode/9router/free-forever` fallback (already policy; must be enforced consistently in CLI, bridge, stream, HTTP modes).
- **Observability**: log `quota.model.disabled_first_seen`, `quota.model.recheck`, `quota.model.reenabled`, `quota.model.fallback_9router`.

---

## 11. API Key Lifecycle

### 11.1 Generation

- Admin creates key via `POST /admin/keys`.
- AgentGate generates random secret (min 32 bytes entropy), returns plaintext once as `agk_<prefix>_<secret>`.
- Store only hash (Argon2id preferred; bcrypt acceptable fallback), prefix, metadata.
- Record `created_at`, `created_by`, optional `expires_at`, `max_uses_total`.

### 11.2 Validation

For each `/v1/*` request:
1. Parse bearer key.
2. Lookup by prefix/index, verify hash.
3. Check `status=active`, not revoked, not expired.
4. Check scopes and allowed provider/model.
5. Check quota/rate/concurrency.
6. Increment `used_count` atomically on accept; update `last_used_at`.

### 11.3 Rotation / revocation

- Rotate: create new key, optionally allow overlap window, revoke old.
- Revoke: set `revoked_at`, `status=revoked`, immediate effect.
- Expire: automatic by `expires_at`.

### 11.4 Audit

Audit events for create/update/revoke/rotate/use-denied/quota-exceeded. Never log plaintext key.

---

## 12. Proxy Single / Pool

### 12.1 Single proxy

- Per-provider `proxy.mode=single`.
- All upstream HTTP traffic uses configured proxy URL.
- Support `http`, `https`; optional SOCKS5 can be phase 2 if reqwest/socks feature enabled.

### 12.2 Proxy pool

- `proxy.mode=pool` with list of endpoints.
- Strategies: `round_robin`, `random`, `least_errors`, `sticky`.
- Health tracking: success/error counts, latency EWMA, last error, banned-until.
- Ban rules: 429/5xx/timeouts trigger temporary cooldown.
- If all proxies unhealthy, fall back to direct (configurable; default false for safety).

### 12.3 Security

- Proxy credentials stored in env/secret file, not committed config.
- Redact proxy user/pass in logs/metrics/admin responses.

---

## 13. Free-Forever Load Balancing

### 13.1 Goal

When a requested model is free/quota-limited/disabled, route across available free models before hard fallback to `opencode/9router/free-forever`.

### 13.2 Algorithm

1. Resolve requested model and policy.
2. If model is quota-disabled or matches free-reroute rule, select candidate pool.
3. Filter candidates by: enabled, provider healthy, key allowed, model not quota-disabled, proxy available.
4. Score by strategy (weighted RR / least latency / least errors).
5. Try selected candidate; on quota/429/5xx/transient, mark cooldown and try next candidate.
6. If pool exhausted, fallback to `opencode/9router/free-forever`.
7. Emit decision log: `from`, `to`, `reason`, `pool`, `attempt`, `fallback`.

### 13.3 Session consistency

- Default `sticky_key=session_id` for free pool to reduce model flapping.
- On model switch due to quota, set `agentgate_force_fresh_cli_session` equivalent for CLI paths; for HTTP mode, do not reuse upstream-stateful session unless explicitly supported.

---

## 14. Observability / Metrics

Add Prometheus metrics:

- `agentgate_http_upstream_requests_total{provider,model,target,result,mode}`
- `agentgate_http_upstream_latency_ms{provider,model}`
- `agentgate_http_upstream_errors_total{provider,model,class}`
- `agentgate_quota_tokens_total{direction,provider,model,key_id}` (`direction=in|out`)
- `agentgate_quota_bytes_total{direction,provider,model,key_id}`
- `agentgate_quota_denied_total{scope,reason}`
- `agentgate_key_usage_total{key_id}`
- `agentgate_proxy_selected_total{provider,proxy,strategy}`
- `agentgate_proxy_errors_total{provider,proxy,class}`
- `agentgate_lb_selection_total{pool,from_model,to_model,reason}`
- `agentgate_lb_fallback_total{pool,to_model,reason}`

Audit log (structured JSON): admin actions, key create/revoke/rotate, provider/model/proxy changes, quota deny, routing failover.

---

## 15. Security

- Never store/log plaintext API keys or upstream provider keys.
- Hash generated AgentGate keys; show plaintext only once.
- Redact `Authorization`, proxy credentials, cookies, secret env values.
- Admin endpoints separate scope from data-plane keys.
- Validate URLs (allow http/https only; block file/private metadata by default where applicable).
- SSRF guard: configurable allowlist for upstream base URLs/proxy targets.
- Rate limit admin endpoints.
- Config file permissions warning for secrets.

---

## 16. Migration Plan

### Phase 0 - Spec & confirmation
- Confirm OpenCode HTTP target and Agnes AI endpoint mapping.
- Freeze config schema v1.

### Phase 1 - Foundation
- Add config structs for HTTP providers, quotas, proxies, pools.
- Add key store abstraction (file-backed JSON first, SQLite optional later).
- Add admin CRUD skeleton with real persistence.

### Phase 2 - HTTP provider runtime
- Implement `HttpOpenAiCompatibleProvider`.
- Wire router mode `cli|http|auto`.
- Support non-stream + stream SSE.

### Phase 3 - Quota/accounting
- Token/byte accounting, per-key/provider/model quotas.
- Key generation/expiry/usage counters.

### Phase 4 - Proxy & LB
- Proxy single/pool, health, cooldown.
- Free-forever pool load balancer + fallback.

### Phase 5 - Hardening
- Tests, metrics, docs, admin UX, rollback flags.

---

## 17. Test Plan

- **Unit:** config parsing, endpoint mapping validation, key hashing/expiry, quota counters, LB selection, proxy health, SSE parser.
- **Integration (mock upstream):** OpenRouter/Groq/Gemini-compatible mock servers; 200/stream/tool_calls/429/5xx/timeout.
- **Regression:** existing CLI bridge tests, quota failover, force-fresh-session-on-reroute.
- **Security:** key plaintext never logged, admin auth required, SSRF/proxy validation.
- **Load:** HTTP mode creates no CLI processes; proxy pool failover; LB distributes free requests.

---

## 18. Open Questions (need user confirmation)

1. **OpenCode provider mapping — PARTIALLY RESOLVED (still open).** Decision made: keep
   `opencode` CLI-only and add a **separate** `opencode-http` provider id; never
   auto-switch. The HTTP base URL remains an **open question**: Go
   (`https://opencode.ai/inference/go/openai/v1`), pay-as-you-go
   (`https://opencode.ai/inference/openai/v1`) or local `opencode serve`. Because
   no target is confirmed, `opencode-http` is intentionally **not registered** in
   the verified mapping registry and stays disabled until an operator sets `base_url`.
2. **Agnes AI mapping — RESOLVED.** User-provided and verified against official docs:
   base `https://apihub.agnes-ai.com/v1`, `POST /chat/completions`, Bearer
   `AGNES_API_KEY`, model `agnes-3.0-flash` (2.5 flash retires 2026-11-30).
3. **Kilo/Kilocode naming — RESOLVED.** Both `kilo` and `kilocode` are accepted
   aliases of the single Kilo AI Gateway mapping.
4. **Quota token source of truth — DECIDED:** trust upstream `usage` first; stream
   final usage chunk second; estimator fallback marked `estimated=true` plus raw
   byte counters (always available).
5. **Key store backend — DECIDED:** SQLite (`src/keystore.rs`, `[auth] key_store_path`,
   default `<config dir>/agentgate/keys.sqlite3`). `[auth] enabled = false` keeps
   legacy static `server.api_keys` behavior; generated keys are hashed.
6. **Proxy scope — DECIDED for phase 1:** HTTP(S) single/pool with health/ban
   tracking via `gateway::proxy` hooks; SOCKS5 is phase 2.
7. **Admin API surface — DECIDED:** same server port with a separate admin scope
   (`[auth]` admin keys / scope), not a second bind. Secrets are redacted in all
   admin responses; existing stub endpoints kept as deprecated aliases.
8. **Free-pool eligibility — OPEN:** only models explicitly tagged `free`, or also
   include `*-free` by name heuristic?
9. **Tool execution in HTTP mode — DECIDED:** pass-through only in phase 1 (client
   executes tools); no AgentGate-managed tool runtime and no CLI spawn.

---

## 19. Implementation Taskboard (proposed)

See Team taskboard created after this spec is accepted/mapped.

---

## 20. Source References

- OpenRouter chat completions: https://openrouter.ai/docs/api-reference/chat-completion
- Groq OpenAI compatibility: https://console.groq.com/docs/openai
- Gemini OpenAI compatibility: https://ai.google.dev/gemini-api/docs/openai
- OpenCode Go docs: https://opencode.ai/docs/go/
- OpenCode Console API/inference: https://opencode.ai/docs/console/api/ , https://opencode.ai/docs/console/api/inference/
- OpenCode harness base URLs: https://opencode.ai/docs/console/harnesses/
- Kilo AI Gateway: https://kilo.ai/docs/gateway
- Kilo AI Gateway auth: https://kilo.ai/docs/gateway/authentication
- Kilo AI Gateway API reference: https://kilo.ai/docs/gateway/api-reference
- AgentGate current foundation: `src/api.rs`, `src/config.rs`, `src/service.rs`, `src/provider.rs`, `src/provider/bridge/*`, `agentgate.toml`
