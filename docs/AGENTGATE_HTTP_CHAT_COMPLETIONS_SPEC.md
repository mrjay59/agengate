# AgentGate HTTP `/chat/completions` Mode Spec

**Status:** Draft  
**Date:** 2026-11-27  
**Scope:** Add an optional HTTP upstream mode alongside existing CLI bridge mode, so AgentGate can serve OpenAI-compatible `/v1/chat/completions` requests through direct HTTP provider backends (including OpenAI-compatible endpoints such as 9router) without spawning a CLI process per turn.

## 1. Problem

Current AgentGate bridge providers (opencode/claude/gemini/ollama-cli) execute CLI processes. This provides strong tool/agent behavior, but has operational costs:

- Process spawn overhead and burst failures (`EAGAIN`, `Broken pipe`, startup crashes).
- Higher RAM/CPU from CLI runtime + MCP child processes.
- More complex cleanup (process groups, orphan MCP servers, temp workdirs).
- Harder scaling for simple chat/completion requests that do not need local CLI tools.

We need an HTTP mode that keeps AgentGate's OpenAI-compatible API while allowing requests to be routed through HTTP `/chat/completions` upstreams.

## 2. Goals

- Support OpenAI-compatible upstreams via HTTP `POST /v1/chat/completions`.
- Keep AgentGate external API unchanged: clients continue calling AgentGate `/v1/chat/completions`.
- Allow per-provider/per-model selection between `cli` and `http` modes.
- Preserve streaming SSE behavior (`text/event-stream`, `[DONE]`).
- Preserve existing tool-call semantics where upstream supports OpenAI tool calls.
- Provide safe fallback/routing policy for free/quota-limited models.
- Improve resource stability by avoiding CLI spawn for HTTP-routed requests.

## 3. Non-Goals

- Replacing CLI bridge mode. CLI remains required for providers/features that depend on local CLI/MCP execution.
- Full parity for every CLI-specific behavior (local filesystem tools, opencode built-in tools, local MCP stdio processes) unless explicitly proxied.
- Implementing provider-specific non-OpenAI protocols in this first spec (except existing adapters).

## 4. User-Facing Behavior

AgentGate receives normal OpenAI-style requests:

```http
POST /v1/chat/completions
Content-Type: application/json
Authorization: Bearer <agentgate-key>
```

AgentGate selects an execution mode based on config/model routing:

- `mode = "cli"`: existing bridge behavior.
- `mode = "http"`: AgentGate forwards request to configured upstream HTTP endpoint and streams/returns normalized response.

The client should not need to change request shape except optional AgentGate extensions already supported.

## 5. Configuration

### 5.1 Provider config

Extend provider config with HTTP mode fields:

```toml
[[provider]]
id = "9router"
type = "openai_compatible_http"   # new
enabled = true
base_url = "https://agentai.autocall.my.id/v1"
api_key_env = "NINE_ROUTER_API_KEY" # or api_key (discouraged in committed config)
default_model = "free-forever"
timeout_secs = 120
max_retries = 2

[provider.http]
chat_completions_path = "/chat/completions"
stream = true
forward_tools = true
forward_tool_choice = true
forward_response_format = true
forward_reasoning = "auto" # auto|passthrough|drop
```

### 5.2 Model routing

Add explicit routing rules:

```toml
[bridge.routing]
default_mode = "cli" # cli|http

[[bridge.routing.rules]]
match_provider = "opencode"
match_model_suffix = "-free"
action = "reroute"
target = "http:9router/free-forever"
reason = "avoid opencode free-limit CLI churn"

[[bridge.routing.rules]]
match_model = "agg/9router/*"
action = "http"
target_provider = "9router"
```

### 5.3 Free-model policy

```toml
[bridge]
reroute_free_models = true
free_model_fallback = "opencode/9router/free-forever"
```

Behavior:
1. If request model is free-tier and reroute enabled, pick configured filtered free model.
2. If no configured free model is available, use `free_model_fallback`.
3. Log routing decision with `from_model`, `to_model`, `reason`.

## 6. Request Normalization

For HTTP mode, AgentGate normalizes inbound request into upstream OpenAI-compatible request:

- `model`: mapped/routed model name.
- `messages`: preserved; tool/tool_calls/reasoning_content preserved when supported.
- `stream`: preserved.
- `tools`, `tool_choice`: forwarded when `forward_tools=true` and upstream supports.
- `temperature/top_p/max_tokens/stop/response_format`: forwarded when set.
- `session_id`, `agentgate_*` internal fields: never forwarded upstream.

If upstream does not support a field, AgentGate drops field with warning and continues when safe.

## 7. Streaming & Response

### 7.1 SSE

- Upstream `text/event-stream` is parsed incrementally.
- AgentGate emits normalized OpenAI-style chunks to client.
- Final `[DONE]` is always emitted on normal completion.
- On upstream error before meaningful output, AgentGate may retry/failover according to policy.

### 7.2 Non-stream

- Upstream JSON response is validated and mapped to AgentGate/OpenAI response shape.
- Ensure `choices[0].message` exists; normalize missing optional fields.

### 7.3 Tool calls

- If upstream emits `tool_calls`, AgentGate passes them through with existing tool-call sanitization/allowlist rules.
- Unknown/unregistered tool calls remain fail-closed as today (drop/clean stop or controlled warning) unless explicitly allowed by policy.

## 8. Error Handling & Fallback

Classify errors:

- **Quota/limit**: 429, `provider.free-limit`, "free usage limit reached".  
  Action: retry backoff, then failover to configured fallback (default `9router/free-forever`) if no meaningful output.
- **Transient network/upstream**: timeout, connection reset, 502/503/504.  
  Action: limited retry with backoff.
- **Invalid request/unsupported feature**: 400/422.  
  Action: no blind retry; return actionable error.
- **Auth/config**: 401/403/missing key.  
  Action: fail fast with clear config error.

All failover decisions must be logged with correlation/request/run IDs.

## 9. Session & State

HTTP mode is stateless by default. AgentGate may maintain logical session metadata for telemetry, but does not rely on persistent CLI child processes.

Options:

- `session_mode = "stateless"` (default): send full conversation each request.
- `session_mode = "upstream_session"` (future): if upstream supports session IDs, map AgentGate session to upstream session.

For tool loops, existing AgentGate run/park/resume semantics apply where possible; if not supported, fall back to replay mode.

## 10. Security

- Never log raw API keys or full Authorization headers.
- Redact sensitive fields in request/response debug logs.
- Enforce allowlist for reroute targets to prevent arbitrary URL injection by client input.
- Keep `allow_client_mcp` semantics: HTTP mode does not execute client-advertised stdio MCP commands on AgentGate host unless explicitly configured by a separate trusted MCP proxy mechanism.

## 11. Observability

Metrics:

- `agentgate_http_requests_total{provider,model,mode,result}`
- `agentgate_http_latency_seconds{provider,model,phase}`
- `agentgate_http_failover_total{from_model,to_model,reason}`
- `agentgate_http_retries_total{provider,reason}`
- `agentgate_http_stream_errors_total{provider,model,type}`

Logs should include: request id, run id, provider, model, routed target, mode, attempt, failover, error class.

## 12. Migration Plan

1. **Phase 1 - Config + plumbing**: add config schema, routing rule evaluation, HTTP provider type skeleton.
2. **Phase 2 - Non-stream HTTP**: implement basic POST/response normalization.
3. **Phase 3 - Streaming SSE**: implement chunk parser, `[DONE]`, error mapping.
4. **Phase 4 - Tool calls/failover**: forward tools, sanitize tool calls, quota/transient failover.
5. **Phase 5 - Observability/hardening**: metrics, redaction, tests, docs.

## 13. Test Plan

- Unit: routing rules, model reroute, error classification, SSE parser.
- Integration: mock OpenAI-compatible upstream for success, 429 failover, 5xx retry, tool_calls stream, malformed SSE.
- Regression: existing CLI bridge tests remain green.
- Load: burst requests should not create CLI processes when routed to HTTP mode.

## 14. Open Questions

- Should HTTP mode support client-advertised remote MCP endpoints (HTTP/SSE) as first-class tools?
- Should AgentGate expose per-request override (`x-agentgate-mode: cli|http`) or keep server-side routing only?
- How should tool execution be handled when upstream returns tool_calls but no local executor is desired in HTTP mode? (pass-through only vs AgentGate-managed tool runtime)
- Do we need per-provider circuit breaker to avoid hammering unhealthy upstreams?
