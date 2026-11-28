# AgentGate

AgentGate is an OpenAI-compatible AI gateway. It exposes HTTP endpoints like `/v1/chat/completions` and `/v1/models`, then routes requests to CLI agents and HTTP providers behind one API.

**Current version:** `0.3.43`

## What it does

- **One OpenAI-style API** for local CLI tools and HTTP model providers.
- **CLI + HTTP gateway mode**: run local agents (for example `opencode`, `copilot`, `gemini`) or connect OpenAI-compatible HTTP providers.
- **Streaming and non-streaming** chat completions.
- **Free-model pool / combo failover**: route across a pool of free models (including 10 free models in the default free-model workflow) when quota/rate limits hit.
- **API key lifecycle**: manage gateway API keys for client access.
- **Proxy, load balancer, and combo routing**: route traffic across upstreams/providers with fallback.
- **Transport hardening**: better handling for transient network/provider errors (retries, classification, safer cleanup).

## Quick start

### 1) Get AgentGate

Use a release binary from [GitHub Releases](https://github.com/mrjay59/agengate/releases), or build from source:

```bash
cargo build --release
```

Binary output:
- Windows: `target\release\agentgate.exe`
- Linux/macOS: `target/release/agentgate`

### 2) Create a config

Copy the example config and edit it:

```bash
# Linux / macOS
cp agentgate.example.toml agentgate.toml

# Windows PowerShell
Copy-Item agentgate.example.toml agentgate.toml
```

Minimum local config:

```toml
[server]
host = "127.0.0.1"
port = 8787
api_keys = ["change-me"] # required before exposing beyond localhost

[[provider]]
id = "opencode"
enabled = true
```

### 3) Start the server

```bash
# Windows
.\agentgate.exe serve --config agentgate.toml

# Linux / macOS
./agentgate serve --config agentgate.toml
```

Server default: `http://127.0.0.1:8787`

## Quick curl examples

> Replace `change-me` with your API key. If `api_keys` is empty and you only bind to `127.0.0.1`, local testing may work without auth depending on your setup; for any shared/networked deployment, set API keys.

### 1. Health check

```bash
curl http://127.0.0.1:8787/health
```

### 2. List models

```bash
curl http://127.0.0.1:8787/v1/models \
  -H "Authorization: Bearer change-me"
```

### 3. Non-streaming chat completion

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer change-me" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/gpt-5-nano",
    "messages": [{"role": "user", "content": "Say hello in one sentence."}],
    "stream": false
  }'
```

### 4. Streaming chat completion

```bash
curl -N http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer change-me" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/gpt-5-nano",
    "messages": [{"role": "user", "content": "Write 3 short bullets about AgentGate."}],
    "stream": true
  }'
```

### 5. Chat with tools (OpenAI tool format)

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer change-me" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/gpt-5-nano",
    "messages": [{"role": "user", "content": "What time is it? Use the tool if needed."}],
    "tools": [
      {
        "type": "function",
        "function": {
          "name": "get_time",
          "description": "Get current time",
          "parameters": {"type": "object", "properties": {}}
        }
      }
    ]
  }'
```

## Config essentials

| Setting | Where | Why it matters |
|---|---|---|
| `server.host` / `server.port` | `agentgate.toml` | Bind address and port. Use `127.0.0.1` for local-only. |
| `server.api_keys` | `agentgate.toml` | Client API keys for `/v1/*`. **Set keys before exposing to network.** |
| `server.request_timeout_secs` | `agentgate.toml` | Overall request timeout. |
| `defaults.max_concurrency_per_provider` | `agentgate.toml` | Limits concurrent requests per provider. |
| `defaults.model_cache_ttl_secs` | `agentgate.toml` | How long model discovery is cached. |
| `[[provider]]` | `agentgate.toml` | Enable/configure CLI or HTTP providers. |
| `NVIDIA_API_KEY` etc. | Environment | Secrets for HTTP providers; keep them out of git. |

See [`agentgate.example.toml`](./agentgate.example.toml) for a full example.

## Documentation

- [INSTALL.md](./INSTALL.md) - platform install guides (Windows, Linux, macOS, Docker, VPS).
- [docs/http_gateway.md](./docs/http_gateway.md) - HTTP gateway overview.
- [docs/TRANSPORT_HARDENING.md](./docs/TRANSPORT_HARDENING.md) - transport errors, retries, diagnostics.
- [docs/spec_free_model_combo.md](./docs/spec_free_model_combo.md) - free-model combo/failover design.
- [docs/PROVIDER_NVIDIA_NIM.md](./docs/PROVIDER_NVIDIA_NIM.md) - NVIDIA NIM provider mapping.
- [docs/BUILD_AND_RELEASE.md](./docs/BUILD_AND_RELEASE.md) - build and release notes.

## Security note

Do **not** bind `server.host = "0.0.0.0"` without configuring `server.api_keys`. If you expose AgentGate on a LAN/VPS/public interface, use strong API keys and put it behind a firewall/reverse proxy as needed.
