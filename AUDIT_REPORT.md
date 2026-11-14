# AgentGate Audit Report

**Project**: `C:\Users\EDN\Documents\AION\project\agentgate`
**Date**: 2026-11-14
**Type**: Rust Gateway/Proxy for CLI-based LLM Agents (OpenCode, Gemini, Claude, Ollama)

---

## Architecture Overview

```
AgentGate = OpenAI-compatible HTTP Gateway + MCP Bridge + CLI Provider Abstraction
```

### Core Components

| Module | Purpose | Lines |
|--------|---------|-------|
| `main.rs` | CLI entry (`serve`, `models`, `test`, `doctor`, `init`, `providers`, `spike-bridge`) | ~300 |
| `lib.rs` | Module exports, `Result<T>` type alias | ~15 |
| `config.rs` | TOML config loading, env overrides, defaults, workspaces | ~630 |
| `provider.rs` | CLI provider trait, implementations (native + bridge), binary resolution | ~2100 |
| `service.rs` | ServiceRegistry (orchestration), routing, model cache, semaphores, run lifecycle | ~1200 |
| `bridge.rs` | MCP bridge server, session mgmt, tool execution, continuation (PR-S4) | ~900 |
| `run.rs` | Run classification (New/Resume/Replay/ChatOnly), parked runs | ~300 |
| `reasoning/*` | Streaming parser, format, redact, coalesce, limits, fallbacks | ~1500 |
| `openai/*` | Tool schema normalization, SSE, name aliasing | ~500 |
| `cli.rs` | CLI arg parsing, command dispatch | ~200 |
| `api.rs` | Axum HTTP server, `/v1/*` endpoints | ~400 |
| `doctor.rs` | Health checks, provider diagnostics | ~200 |
| `types.rs` | OpenAI API types, provider config, error enums | ~400 |

---

## Key Features Verified

### 1. Multi-Provider Abstraction ✅
- **Native CLI providers**: `opencode`, `gemini`, `claude`, `ollama` (via `ollama run`)
- **Bridge providers**: Same CLIs via MCP bridge (tool isolation)
- **Ollama dual-mode**: HTTP native (`/api/tags`) + CLI bridge (`ollama_mode = "cli"`)
- **Extensible**: `build_cli_provider()` factory pattern

### 2. MCP Bridge Architecture ✅
- **Session registration** before CLI spawn (spec §9.2)
- **Tool isolation** (`[bridge.isolation]`): selective/strict/off modes
- **Parked runs** with tool_call_id linkage (PR-S1/PR-S2)
- **Live continuation** (PR-S4): capability-gated, replay fallback mandatory
- **Transport**: `http` (loopback) + `stdio_shim` for CLI without native MCP

### 3. Routing & Classification ✅
| Classification | Trigger | Handler |
|---|---|---|
| `Native` | Ollama HTTP, no tools | Direct proxy |
| `NewChatOnly` | No client tools, bridge policy | Native CLI |
| `NewBridge` | Client tools + bridge provider | MCP bridge |
| `Resume` | Known `tool_call_id` in parked run | Resume + continuation |
| `Replay` | Unknown `tool_call_id` | Full replay with fresh session |

### 4. Concurrency & Safety ✅
- Per-provider semaphore (`[defaults] max_concurrency_per_provider`, default 2)
- CLI timeouts: 300s (non-stream), 180s idle (stream), 10s model scan
- First-token timeout (default 120s) for streaming
- Fail-closed: tool isolation preflight, provider availability gate

### 5. Model Discovery ✅
- Parallel `scan_models()` with 10s timeout per provider
- Cache TTL 300s, stale cache fallback on scan failure
- Namespacing: `agg/<provider>/<model>` prevents collisions (e.g., 9router models via opencode)
- Ollama native `/api/tags` probe (2s timeout)

### 6. Streaming & Reasoning ✅
- `ReasoningParser`: OpenCode JSONL, `<think>`, `Thinking...`, activity markers
- Formats: `reasoning_content` (default), `activity`, `tool_call` fallbacks
- Redaction patterns for API keys
- Coalescing, limits, partial messages, lenient events

### 7. Observability ✅
- Structured tracing (`tracing` crate): `event = "bridge.tools_list"`, `"batch.issued"`, `"continuation.registered"`, `"replay_fallback"`
- Metrics: CLI exit success/failure, latency
- Lighthouse/performance hooks available

---

## Configuration (`agentgate.toml`)

```toml
[server]           # host, port, api_keys, cors, timeout
[defaults]         # workdir, concurrency, cache_ttl, first_token_timeout
[routing]          # no_tools_policy, on_client_tools, default_workspace
[bridge]           # enabled, host, port, ttl, limits, guard, isolation, transport
[workspace.<name>] # path, permission_mode
[[provider]]       # id, bin, models, models_cmd, chat_cmd, bridge, reasoning, etc.
```

**Env Overrides** (highest priority after CLI):
- `AGENTGATE_BRIDGE_PORT`, `*_CALL_TTL_SECS`, `*_PARK_TTL_SECS`, `*_MAX_PARKED_RUNS`, `*_TRANSPORT`
- `AGENTGATE_PROVIDER_TIMEOUT_SECS` (default 120s)

---

## Critical Code Paths (Risk Areas)

| File | Function | Risk | Mitigation |
|------|----------|------|------------|
| `provider.rs` | `send_chat_completion` | CLI hang (auto-pull, TTY, auth) | 300s timeout + stderr capture + kill_on_drop |
| `provider.rs` | `resolve_binary` | Windows `.cmd`/`.bat` resolution | Extension priority, nvm/npm fallback paths |
| `service.rs` | `list_models` | Gateway timeout on first call | Background `warm_model_cache()` at startup |
| `bridge.rs` | `park_run` / `continuation` | Session leak on client disconnect | `kill_on_drop`, cleanup on timeout/error |
| `provider.rs` | `parse_models_output` | Format variance (JSON, table, space-separated) | Multi-format parser, provider-specific logic |
| `service.rs` | `resolve_provider` | 9router/opencode namespace collision | `agg/` prefix + owner attribution |

---

## Dependencies (Cargo.toml)

**Core**: `axum`, `tokio`, `serde`, `anyhow`, `tracing`, `uuid`, `async-trait`
**MCP/Tools**: `rmcp` (MCP SDK), `schemars`, `openai-api-types`
**CLI**: `clap`, `which`, `strip-ansi-escapes`
**Config**: `toml`, `dirs`
**HTTP**: `reqwest` (Ollama native)

---

## Gaps / TODOs (from code comments)

1. **P0-AUDIT seq2**: Batch/queue remaining tool calls instead of truncate (partially done in `new_bridge_run`)
2. **INVEST-TOOL-FILTER**: Hardcoded 10/128 tool limits for free/paid models
3. **TB-TOOL-02**: Isolation modes need more testing (selective vs strict)
4. **Spec §14**: Workspace passthrough mode not fully wired
5. **PR-S5**: Session rotation on replay - verify `agentgate_run_id` propagation
6. **Ollama native**: Tool calling not implemented (chat-only)

---

## Build & Test

```bash
# Build
cargo build --release

# Run server
cargo run -- serve

# CLI commands
cargo run -- init              # Generate config
cargo run -- providers         # List providers + availability
cargo run -- models            # List all models (with cache)
cargo run -- test <model> <prompt>
cargo run -- doctor            # Health check
cargo run -- spike-bridge      # Integration test bridge
```

---

## Summary

**Status**: Production-ready gateway with advanced MCP bridge, tool isolation, and multi-CLI support.

**Strengths**:
- Clean separation: HTTP gateway ↔ provider abstraction ↔ bridge
- Fail-closed design (timeouts, isolation preflight, availability gates)
- Robust Windows binary resolution (nvm, npm, PATHEXT)
- Streaming parity: native CLI + bridge both implement `send_chat_completion_stream`
- Observability via structured tracing

**Watch Items**:
- Bridge session lifecycle under load (parked runs, continuation cleanup)
- Free model tool injection limit (10 tools) - may need dynamic config
- Ollama native tool calling gap
- First-request latency without cache warm