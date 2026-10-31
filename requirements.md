# Requirements Specification: agentgate

## Project Overview
**Name**: agentgate  
**Type**: Single binary Rust HTTP gateway  
**Purpose**: Expose locally installed CLI agents (opencode, GitHub Copilot CLI, Gemini CLI, etc.) as a unified OpenAI-compatible HTTP endpoint (`/v1/...`)

---

## Core Requirements

### Functional Requirements

| ID | Requirement | Priority |
|----|-------------|----------|
| FR-01 | Detect installed CLI agents in PATH at startup and on demand (`/admin/refresh`) | P0 |
| FR-02 | Read model lists from each CLI provider | P0 |
| FR-03 | Accept OpenAI-compatible requests: `GET /v1/models`, `GET /v1/models/{id}`, `POST /v1/chat/completions` (stream & non-stream) | P0 |
| FR-04 | Route requests to correct provider based on model prefix (`<provider>/<model-id>`) | P0 |
| FR-05 | Execute CLI as subprocess in non-interactive mode, capture stdout/stderr | P0 |
| FR-06 | Translate CLI output to OpenAI format (JSON for non-stream, SSE for streaming) | P0 |
| FR-07 | Support cancellation via `CancellationToken` when client disconnects | P0 |
| FR-08 | Provide admin endpoints: `GET /admin/providers`, `POST /admin/refresh` | P0 |
| FR-09 | Generic provider adapter via TOML config (no recompile needed for new CLIs) | P1 |
| FR-10 | CLI provider adapters: opencode, copilot, gemini (claude_code, codex optional) | P0/P1 |
| FR-11 | Health check endpoint `GET /health` returning `{"status":"ok"}` | P0 |
| FR-12 | Root endpoint `GET /` returning service info + detected providers | P1 |

### Non-Functional Requirements

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-01 | Single static binary distribution (Linux x86_64/aarch64, Windows x86_64, macOS bonus) | P0 |
| NFR-02 | Default bind: `127.0.0.1:8787` (localhost only) | P0 |
| NFR-03 | Request timeout configurable, default 5 min | P0 |
| NFR-04 | Concurrency limit per provider (semaphore), default 2 | P0 |
| NFR-05 | Model cache TTL 5 minutes | P0 |
| NFR-06 | Body size limit 4 MB | P0 |
| NFR-07 | Bearer token auth middleware (required for non-loopback) | P1 |
| NFR-08 | No prompt logging by default (`log_prompts = false`) | P0 |
| NFR-09 | Secret redaction in logs/stderr | P0 |
| NFR-10 | Cross-platform: Windows `.cmd` shim handling, arg escaping, process tree kill | P0 |
| NFR-11 | UTF-8 enforced, ANSI stripping fallback | P0 |

---

## Target Audience
- **Primary**: Developers using AI coding assistants (Cline, Continue, OpenWebUI, custom OpenAI SDK clients)
- **Secondary**: Teams wanting unified local LLM access without vendor API keys
- **Environment**: Local development machines, CI/CD runners, self-hosted environments

---

## Supported CLI Providers (v1)

| Provider | Binary | Model List Method | Chat Command |
|----------|--------|-------------------|--------------|
| opencode | `opencode` | `opencode models` | `opencode run --model <prov/model> "<prompt>"` |
| copilot | `copilot` | Static config | `copilot -p "<prompt>" --model <model>` |
| gemini | `gemini` | Static config | `gemini -p "<prompt>" -m <model>` |
| claude_code | `claude` | Static config | `claude -p "<prompt>" --output-format stream-json` |
| codex | `codex` | Static config | `codex exec "<prompt>"` |

---

## Model Naming Convention
```
<provider>/<model-id>
Examples:
- opencode/anthropic/claude-sonnet-4
- opencode/openai/gpt-5
- copilot/gpt-5
- gemini/gemini-2.5-pro
```

---

## Configuration (TOML)

```toml
[server]
host = "127.0.0.1"
port = 8787
auth_token = ""  # optional bearer token

[defaults]
workdir = "."
max_concurrency_per_provider = 2
model_cache_ttl_secs = 300
request_timeout_secs = 300
max_body_bytes = 4194304
log_prompts = false

[provider.opencode]
enabled = true
# bin = "/custom/path/opencode"

[provider.copilot]
enabled = true
models = ["gpt-5", "claude-sonnet-4"]

[provider.gemini]
enabled = true
models = ["gemini-2.5-pro", "gemini-2.5-flash"]

# Generic provider template
[[provider]]
id = "mycli"
bin = "mycli"
models_cmd = ["models"]
chat_cmd = ["run", "--model", "{model}", "{prompt}"]
chat_stdin = false
```

---

## API Contract

### GET `/v1/models`
```json
{
  "object": "list",
  "data": [
    {"id": "opencode/anthropic/claude-sonnet-4", "object": "model", "owned_by": "opencode"},
    {"id": "copilot/gpt-5", "object": "model", "owned_by": "copilot"}
  ]
}
```

### POST `/v1/chat/completions`
**Request**:
```json
{
  "model": "opencode/anthropic/claude-sonnet-4",
  "messages": [{"role": "user", "content": "Hello"}],
  "stream": true,
  "temperature": 0.7,
  "max_tokens": 4096
}
```

**Non-stream Response**: OpenAI `ChatCompletion` format  
**Stream Response**: SSE with `chat.completion.chunk` events + `[DONE]`

---

## Edge Cases & Error Handling

| Scenario | Handling |
|----------|----------|
| Provider binary not found | Return 404 with OpenAI error format |
| CLI exit code ≠ 0 | Map to 502 `upstream_error`, include truncated stderr |
| Client disconnect during streaming | Cancel subprocess via `CancellationToken` |
| Windows `.cmd` arg length > 8191 | Use stdin or temp file for prompt |
| ANSI color codes in output | Strip via `NO_COLOR=1`, `TERM=dumb`, fallback `strip-ansi-escapes` |
| Invalid UTF-8 in stdout | Use `from_utf8_lossy` |
| Provider timeout | Kill process tree, return 504 |
| Auth token missing on non-loopback | 401 Unauthorized |

---

## Security Requirements

1. **No credential storage** — agentgate never stores CLI login credentials
2. **Localhost-only default** — bind to `127.0.0.1` by default
3. **Bearer auth** — required when binding to non-loopback interfaces
4. **Prompt redaction** — never log prompt content unless `log_prompts=true`
5. **Secret scanning** — redact tokens/keys from stderr before logging
6. **Input validation** — validate model names, limit body size, sanitize paths

---

## Observability

- Structured logging via `tracing` (JSON in production)
- Key events: request received, provider selected, CLI spawned, response sent, errors
- No prompt content in logs by default
- Admin endpoints for provider status visibility

---

## Milestones & Deliverables

| Milestone | Scope | Target |
|-----------|-------|--------|
| M1 | Core server, provider registry, opencode adapter, non-stream chat | 3 days |
| M2 | SSE streaming, cancellation, concurrency limits, ANSI stripping | 2 days |
| M3 | Copilot, Gemini adapters, generic adapter, model cache, `/admin/refresh` | 2-3 days |
| M4 | Bearer auth, `doctor/providers/models/test` CLI, log redaction | 2 days |
| M5 | Cross-build, release binary, systemd/NSSM, README, integration examples | 1-2 days |

---

## Testing Strategy

1. **Unit tests**: model parsing, message→prompt conversion, SSE chunk formatting, ANSI strip, JSONL parser
2. **Integration tests**: fake CLI scripts (sh + .cmd), full HTTP request/response cycles
3. **E2E tests**: curl/OpenAI SDK against running server with real CLIs (manual)
4. **Cross-platform**: CI on Linux + Windows

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| CLI flag/output changes | Adapter breaks | Config templates, `doctor` version check, fake CLI tests, pin supported versions |
| High latency (spawn per request) | Slow responses | `opencode serve` persistent mode (future), process pooling (future) |
| Unstructured CLI output | Broken streaming, no usage | Prefer JSON mode, estimate usage when absent |
| CLI interactive prompts | Hanging requests | Non-interactive flags, timeouts, `doctor` login check |
| Windows `.cmd` escaping | Security/bugs | Stdin/temp file for prompts, strict validation |