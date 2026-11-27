# NVIDIA NIM provider mapping (AgentGate)

**Provider id:** `nvidia`
**Status:** Verified mapping (spec §6.1)
**Mapping source:** <https://docs.api.nvidia.com/nim/reference/llm-apis>

## Endpoint mapping

| Field | Value |
|---|---|
| Provider type | `http_openai_compatible` |
| Mode | `http` (no CLI spawn; `bridge = false`) |
| Adapter | `openai_compatible_chat` |
| Base URL | `https://integrate.api.nvidia.com/v1` |
| Chat completions | `POST /chat/completions` |
| Model catalog | `GET /models` |
| Auth | `Authorization: Bearer <NVIDIA_API_KEY>` (`nvapi-*` prefix) |
| Config key source | `NVIDIA_API_KEY` env var (preferred); inline `api_key` discouraged |
| Model IDs | NIM catalog IDs, e.g. `meta/llama-3.3-70b-instruct` |
| AgentGate model prefix | `agg/nvidia/<upstream-model>` |

The provider is **disabled by default** in `agentgate.toml` / `agentgate.example.toml`
because it requires a key. Enable it explicitly after setting `NVIDIA_API_KEY`.

## Verification against the official API reference

Facts below were read from the NIM LLM API reference (OpenAPI definitions for
the hosted models), not guessed:

### Chat completions

- The server URL is `https://integrate.api.nvidia.com/v1/` and the operation is
  `POST /chat/completions`.
- Auth scheme is HTTP bearer (`securitySchemes.Token`), key format `nvapi-*`.
- Request fields declared by the NIM `ChatRequest` schema: `model`, `messages`,
  `temperature`, `top_p`, `tools`, `frequency_penalty`, `presence_penalty`,
  `max_tokens`, `stream`, `stop`. `messages` is required.
- Important: `ChatRequest` sets `additionalProperties = false`. Fields that are
  **not** declared (notably `tool_choice`, `response_format`, `seed`, `n`) can
  produce `422 Unprocessable Entity`. AgentGate therefore sets
  `forward_tool_choice = false` and `forward_response_format = false` for this
  provider. `forward_tools = true` is safe because `tools` is declared.

### Model catalog sync

- The documented catalog lists model IDs such as `meta/llama-3.3-70b-instruct`,
  `meta/llama-3.1-8b-instruct`, `qwen/qwen3-next-80b-a3b-instruct`,
  `deepseek-ai/deepseek-v4-flash`, etc.
- `models_path = "/models"` is configured for OpenAI-compatible catalog sync
  (`GET https://integrate.api.nvidia.com/v1/models`). Config also seeds a small
  static catalog (`NVIDIA_SEED_MODELS`) so `/v1/models` has entries before a live
  sync. The live endpoint must be validated with a real key during enablement;
  if it is unavailable, the seed list is used instead.

### Streaming

- `stream: true` produces OpenAI-style data-only SSE: `data: {...}\n\n` chunks,
  terminated by `data: [DONE]`.
- Response schema `ChatCompletionChunk` carries `choices[].delta` (role/content/
  tool_calls) and `finish_reason` (`stop` | `length` | `tool_calls`).
- The HTTP runtime must forward SSE frames, preserve tool-call deltas, and
  never failover after meaningful output has been emitted.
- `stream_idle_timeout_secs = 120` bounds a stalled upstream stream.

### Tool calls

- `tools` accepts `{type: "function", function: {name, description, parameters}}`.
- `tool_calls` appear on assistant messages; `finish_reason = "tool_calls"` is
  documented. Both non-stream and streaming (`delta.tool_calls`) paths are
  supported.
- `tool_choice` is not in the declared schema for several models and is not
  forwarded by default (see 422 note above).

### Usage / accounting

- Non-stream responses include `usage` with `prompt_tokens`, `completion_tokens`,
  `total_tokens`. Stream responses may include a final usage chunk; if absent,
  the accounting layer falls back to its estimated token counter (marked
  `estimated=true`) plus raw byte counters.

### Quota / rate-limit behavior

- Documented invocation errors: `422` (validation) and `500` (invocation error),
  both with `{type, title, status, detail, instance, requestId}`.
- Rate-limit / quota exhaustion is surfaced as HTTP `429`. The mapping declares
  `quota_exhausted_statuses = [429]` so the router classifies it as
  `rate_limit` and applies the configured retry/backoff (`max_retries = 2`,
  `retry_backoff_ms = [250, 1000]`) followed by failover to the next combo
  candidate — never after meaningful stream output.
- Because per-key NIM quotas are not published in the API reference, AgentGate
  does not hardcode numeric limits. Local per-key/provider quotas come from the
  gateway quota config, not from assumed upstream numbers.

## Code locations

- `src/provider/mappings.rs` — verified mapping data, `nvidia()`, `resolve()`,
  key validation, `to_provider_config()`, unit tests.
- `src/config.rs` — HTTP provider fields on `ProviderConfig` and the generated
  default config block.
- `agentgate.toml`, `agentgate.example.toml` — operator-facing provider block.
