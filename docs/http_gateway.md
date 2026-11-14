# AgentGate HTTP Gateway — Definition & Plan

## Overview

AgentGate acts as an **HTTP gateway** exposing a unified OpenAI-compatible `/v1/chat/completions` endpoint that routes requests to multiple backend LLM providers (opencode, gemini, ollama, custom CLIs) via:

- **Bridge mode** — MCP (Model Context Protocol) over HTTP/stdio for tool-calling CLIs
- **Native mode** — Direct CLI execution with streaming output

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        AgentGate HTTP Gateway                    │
│  ┌──────────────────┐    ┌──────────────────┐                   │
│  │  OpenAI API      │    │  Admin API       │                   │
│  │  /v1/chat/...    │    │  /admin/...      │                   │
│  └────────┬─────────┘    └────────┬─────────┘                   │
│           │                       │                              │
│           ▼                       ▼                              │
│  ┌──────────────────────────────────────────┐                   │
│  │           ServiceRegistry                │                   │
│  │  - Provider registry (opencode, gemini,  │                   │
│  │    ollama, custom)                       │                   │
│  │  - BridgeServer (MCP sessions, tool      │                   │
│  │    whitelisting, isolation guards)       │                   │
│  │  - Model cache (TTL 300s, stale fallback)│                   │
│  └────────┬────────────────────────────────┘                   │
│           │                                                    │
│     ┌─────┴─────┬─────────────┐                                │
│     ▼           ▼             ▼                                │
│ ┌───────┐ ┌──────────┐ ┌────────────┐                          │
│ │opencode│ │ gemini   │ │ ollama     │ ...                     │
│ │ Bridge │ │ Bridge   │ │ Native     │                          │
│ └───────┘ └──────────┘ └────────────┘                          │
└─────────────────────────────────────────────────────────────────┘
```

---

## Core Gateway Features

### 1. Provider Abstraction
- **Unified interface**: All providers implement `CliProvider` trait
- **Model discovery**: `opencode models`, `gemini list`, `ollama list` via `scan_models()`
- **Model cache**: 300s TTL with stale-cache fallback on scan failure

### 2. Bridge Mode (MCP)
- **Transport**: HTTP (`/bridge/mcp/:session_id`) or stdio_shim
- **Tool schema injection**: Client-provided tools → MCP server
- **Isolation guards**: Selective mode — deny conflicting built-ins (bash, write, read, execute, edit, etc.)
- **Per-session auth**: Bearer token per MCP session (not global API key)

### 3. OpenAI Compatibility
- **Endpoints**: `/v1/models`, `/v1/chat/completions`
- **Streaming**: SSE (`stream: true`) with reasoning_content delta
- **Tool calling**: OpenAI function calling format with name aliasing
- **Reasoning**: `reasoning_content` field in delta

### 4. Auth Model
| Route | Auth Method |
|-------|-------------|
| `/v1/*` | Global API key (`api_keys` config) |
| `/admin/*` | Global API key |
| `/_bridge/mcp/*` | **Per-session token** (excluded from global auth) |
| `/health`, `/` | None |

---

## Current Implementation Status (v0.2.6+)

| Feature | Status | Notes |
|---------|--------|-------|
| OpenAI `/v1/chat/completions` | ✅ | Streaming + non-stream |
| `/v1/models` | ✅ | Aggregated from all providers |
| Bridge MCP HTTP | ✅ | `/_bridge/mcp/:session_id` |
| Bridge stdio_shim | ✅ | Fallback transport |
| Tool name normalization | ✅ | `Shell`→`bash`, `path`→`file_path` |
| Model cache + stale fallback | ✅ | 300s TTL, keeps stale on scan fail |
| Selective isolation | ✅ | Denies conflicting built-ins |
| Free-model tool limit | ✅ | ≤10 tools injected |
| Global auth bypass for bridge | ✅ | `/_bridge/*` excluded |

---

## Plan: HTTP Gateway Enhancements

### Phase 1: Observability & Reliability (Week 1)
- [ ] **Structured logging** for gateway requests (request_id, provider, model, latency, status)
- [ ] **Metrics**: Prometheus counters for requests, errors, tool calls, bridge holds
- [ ] **Health checks**: Per-provider availability endpoint (`/health/providers`)
- [ ] **Request tracing**: OpenTelemetry integration

### Phase 2: Gateway Policies (Week 2)
- [ ] **Rate limiting**: Per-key / per-model token budgets
- [ ] **Model routing rules**: Fallback chains (e.g., `beam-free` → `beam` on rate limit)
- [ ] **Request validation**: Schema validation for tool calls before bridge
- [ ] **Timeout policies**: Configurable per-model timeouts

### Phase 3: Multi-tenancy (Week 3)
- [ ] **Workspaces**: Isolated tool schemas / provider configs per workspace
- [ ] **API key scopes**: Per-key provider/model restrictions
- [ ] **Usage tracking**: Token counts per key/model for billing/quotas

### Phase 4: Advanced Routing (Week 4)
- [ ] **Smart routing**: Cost/latency-aware model selection
- [ ] **Streaming proxy**: Direct passthrough for providers with native SSE
- [ ] **Batch API**: `/v1/batch` for async job processing
- [ ] **Webhook callbacks**: Async completion notifications

### Phase 5: Developer Experience (Ongoing)
- [ ] **OpenAPI spec**: Auto-generated from code
- [ ] **Playground UI**: Built-in `/playground` for testing
- [ ] **CLI client**: `agentgate-cli` for local development
- [ ] **Integration docs**: Examples for AionUI, custom clients

---

## Configuration Reference

```toml
# agentgate.toml
[server]
host = "0.0.0.0"
port = 8780
api_keys = ["default_key"]           # Global API keys
request_timeout_secs = 600
cors = true

[defaults]
workdir = "."
max_concurrency_per_provider = 10
model_cache_ttl_secs = 300           # Cache TTL

[bridge]
isolation_mode = "selective"         # "selective" | "permissive" | "strict"
minimal_free_model = true            # Skip opencode.json tools for *-free models
preserve_client_system_prompt = false

[[provider]]
id = "opencode"
bin = "opencode"
models_cmd = ["models"]
chat_cmd = ["run", "--model", "{model}", "--thinking", "--format", "json", "{prompt}"]
strip_ansi = true
enabled = true
models = ["opencode/beam-free", "opencode/flint-40b-free", "opencode/deepseek-v4.1-flash-free"]
transport = "stdio_shim"             # "http" | "stdio_shim"

[[provider]]
id = "gemini"
bin = "gemini"
models_cmd = ["list", "--format", "json"]
chat_cmd = ["generate", "--model", "{model}", "--prompt-stdin"]
enabled = true
models = ["gemini-2.5-pro", "gemini-2.5-flash"]
```

---

## API Examples

### Chat Completion (Non-streaming)
```bash
curl -X POST http://localhost:8780/v1/chat/completions \
  -H "Authorization: Bearer default_key" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "agg/9router/free-forever",
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": false
  }'
```

### Chat Completion (Streaming)
```bash
curl -X POST http://localhost:8780/v1/chat/completions \
  -H "Authorization: Bearer default_key" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "agg/opencode/beam-free",
    "messages": [{"role": "user", "content": "Explain quantum computing"}],
    "stream": true
  }'
```

### With Tools
```bash
curl -X POST http://localhost:8780/v1/chat/completions \
  -H "Authorization: Bearer default_key" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "agg/opencode/beam-free",
    "messages": [{"role": "user", "content": "List files in /tmp"}],
    "tools": [
      {"type": "function", "function": {"name": "shell", "description": "Run shell command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}}}
    ],
    "stream": false
  }'
```

---

## Deployment

### Local (Windows)
```powershell
cargo build --release
./target/release/agentgate.exe serve --port 8780
```

### VPS (Linux systemd)
```bash
# Build
cargo build --release
cp target/release/agentgate /usr/local/bin/

# systemd service
cat > /etc/systemd/system/agentgate.service <<'EOF'
[Unit]
Description=AgentGate HTTP Gateway
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root/agentgate-build
ExecStart=/usr/local/bin/agentgate serve --port 8780
Restart=on-failure
RestartSec=5
Environment=PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now agentgate
```

---

## Related Files

| File | Purpose |
|------|---------|
| `src/main.rs` | HTTP server, routing, global auth middleware |
| `src/api.rs` | `/v1/*` handlers, bridge MCP endpoints |
| `src/bridge.rs` | BridgeServer, session management, tool normalization |
| `src/service.rs` | Provider registry, model cache, request routing |
| `src/provider/*.rs` | Provider implementations (opencode, gemini, ollama, bridge) |
| `agentgate.toml` | Configuration |

---

## References

- [OpenAI API Spec](https://platform.openai.com/docs/api-reference)
- [MCP Specification](https://modelcontextprotocol.io/specification)
- [AgentGate Plan](plan.md) — Streaming migration plan
- [Reasoning Spec](reasoning.md) — Reasoning content format