# AgentGate Audit Report

**Project**: `C:\Users\EDN\Documents\AION\project\agentgate`
**Date**: 2026-11-18
**Type**: Rust OpenAI-compatible Gateway + MCP Bridge for CLI-based LLM Agents
**Version**: 0.1.0

---

## Architecture Overview

```
Client (AionUI/Copilot/OpenAI SDK)
    │
    ▼
┌─────────────────────────────────────────────────────┐
│  AgentGate (Axum HTTP :8787)                        │
│  /v1/models  /v1/chat/completions  /_bridge/mcp/*   │
└──────────────┬──────────────────────────────────────┘
               │
    ┌──────────▼──────────┐
    │  ServiceRegistry    │  routing, model cache,
    │  (orchestration)    │  semaphores, run lifecycle
    └──────┬───┬───┬──────┘
           │   │   │
    ┌──────▼┐ ┌▼──┐ ┌▼──────────┐
    │Bridge │ │CLI│ │Ollama     │
    │(MCP)  │ │   │ │Native HTTP│
    └──┬────┘ └─┬─┘ └───────────┘
       │        │
  ┌────▼────┐  ┌▼──────────┐
  │OpenCode │  │Gemini     │
  │Claude   │  │Ollama CLI │
  └─────────┘  └───────────┘
```

### Module Map (38 files, 7 directories)

| Module | Files | Purpose |
|--------|-------|---------|
| `main.rs` + `cli.rs` | 2 | CLI entry (`serve`, `models`, `test`, `doctor`, `init`, `providers`, `spike-bridge`) |
| `lib.rs` | 1 | Crate root, module declarations |
| `config.rs` | 1 | TOML config, env overrides, defaults (~640 lines) |
| `types.rs` | 1 | OpenAI API types, provider config, error enums (~304 lines) |
| `error.rs` | 1 | AppError enum → HTTP status mapping (~131 lines) |
| `service.rs` | 1 | ServiceRegistry: routing, model cache, semaphores, run lifecycle (~1231 lines) |
| `bridge.rs` | 1 | BridgeServer: MCP sessions, parked runs, continuations, tool execution (~1654 lines) |
| `run.rs` | 1 | Run classification (Native/Resume/Replay/NewBridge/NewChatOnly), RunRegistry (~492 lines) |
| `api.rs` | 1 | Axum router: `/v1/*`, `/_bridge/*`, `/admin/*`, `/health`, `/_metrics` (~1102 lines) |
| `metrics.rs` | 1 | Prometheus metrics |
| `doctor.rs` | 1 | Health check diagnostics |
| `permission/mod.rs` | 1 | Policy enums (NoToolsPolicy, ClientToolsPolicy, etc.) |
| `provider.rs` | 1 | CliProvider trait + CliProviderImpl + BridgeCliProvider (~2060 lines) |
| `provider/bridge.rs` | 1 | Bridge factory, BridgeSession trait |
| `provider/bridge/opencode.rs` | 1 | OpenCode MCP adapter (~1229 lines) |
| `provider/bridge/gemini.rs` | 1 | Gemini CLI adapter |
| `provider/bridge/claude.rs` | 1 | Claude CLI adapter |
| `provider/bridge/ollama_cli.rs` | 1 | Ollama CLI adapter |
| `provider/bridge/shim.rs` | 1 | Stdio MCP shim |
| `provider/bridge/spike.rs` | 1 | Integration test spike |
| `provider/ollama_native.rs` | 1 | Ollama HTTP native proxy |
| `provider/ollama_cli.rs` | 1 | Ollama CLI provider |
| `openai/tools.rs` | 1 | Tool schema normalization, aliasing, `$ref` inlining (~975 lines) |
| `openai/sse_tools.rs` | 1 | SSE streaming tool call chunks |
| `reasoning/mod.rs` | 1 | Module exports |
| `reasoning/event.rs` | 1 | RunEvent enum (Text, Reasoning, ToolCalls, etc.) |
| `reasoning/parser.rs` | 1 | Tolerant JSONL parser (Claude/OpenCode/Gemini/Ollama) (~805 lines) |
| `reasoning/pipeline.rs` | 1 | Reasoning processing pipeline |
| `reasoning/limit.rs` | 1 | Hard limits for reasoning output |
| `reasoning/format.rs` | 1 | Reasoning format (reasoning_content, think tags, etc.) |
| `reasoning/coalesce.rs` | 1 | Delta coalescing |
| `reasoning/fallback.rs` | 1 | Activity fallback |
| `reasoning/redact.rs` | 1 | API key redaction |
| `reasoning/tool_view.rs` | 1 | Tool call view formatting |
| `bin/fake_cli_*.rs` | 2 | Test fixtures |

---

## Verified Features

### 1. Multi-Provider Abstraction ✅
- **Native CLI**: `opencode`, `gemini`, `claude`, `ollama` (via `ollama run`)
- **Bridge (MCP)**: Same CLIs with tool isolation
- **Ollama dual-mode**: HTTP native (`/api/tags`) + CLI bridge (`ollama_mode = "cli"`)
- **Factory pattern**: `build_cli_provider()` selects bridge vs native

### 2. MCP Bridge Architecture ✅
- **Session registration** before CLI spawn (spec §9.2)
- **Tool isolation** (`[bridge.isolation]`): selective / strict / off modes
- **Parked runs** with `tool_call_id` linkage (PR-S1, PR-S2)
- **Live continuation** (PR-S4): capability-gated, replay fallback mandatory
- **Transport**: `http` (loopback) + `stdio_shim` for CLIs without native MCP
- **Config injection**: `opencode.json` in session workdir (v2.x compatible)
- **Free model handling**: minimal config for `*-free` models (avoids `provider.free-limit`)

### 3. Routing & Classification ✅
| Classification | Trigger | Handler |
|---|---|---|
| `Native` | Ollama HTTP, no tools | Direct proxy |
| `NewChatOnly` | No client tools, bridge policy | Native CLI |
| `NewBridge` | Client tools + bridge provider | MCP bridge |
| `Resume` | Known `tool_call_id` in parked run | Resume + continuation |
| `Replay` | Unknown `tool_call_id` | Full replay with fresh session |

### 4. Concurrency & Safety ✅
- Per-provider semaphore (`max_concurrency_per_provider`, default 2)
- CLI timeouts: 300s (non-stream), 180s idle (stream), 10s model scan
- First-token timeout (default 120s) for streaming
- Fail-closed: tool isolation preflight, provider availability gate, tool whitelist
- Session cleanup: orphan PID sweep, workdir cleanup on close

### 5. Model Discovery ✅
- Parallel `scan_models()` with 10s timeout per provider
- Cache TTL 300s, stale cache fallback on scan failure
- Namespacing: `agg/<provider>/<model>` prevents collisions
- Ollama native `/api/tags` probe (2s timeout)

### 6. Streaming & Reasoning ✅
- `ReasoningParser`: OpenCode JSONL, Claude stream-json, Gemini, Ollama inline
- Formats: `reasoning_content` (default), `activity`, `tool_call` fallbacks
- Redaction patterns for API keys
- Coalescing, limits, partial messages, lenient events
- Early role chunk + keep-alive comments for TTFB

### 7. Observability ✅
- Structured tracing (`tracing` crate): `event = "bridge.tools_list"`, `"batch.issued"`, etc.
- Prometheus metrics endpoint (`/_metrics`)
- `x-agentgate-run`, `x-agentgate-mode`, `x-agentgate-replay` headers

---

## Configuration Reference

```toml
[server]           # host, port, api_keys, cors, timeout
[defaults]         # workdir, concurrency, cache_ttl, first_token_timeout
[routing]          # no_tools_policy, on_client_tools, default_workspace, model_aliases
[bridge]           # enabled, host, port, ttl, limits, guard, isolation, transport
[bridge.batch]     # window_ms, max_calls
[bridge.resume]    # replay, partial_grace_ms, allow_retry
[bridge.limits]    # max_tools, max_schema_bytes, max_result_bytes, max_prompt_chars
[bridge.guard]     # enforce, preflight_probe
[bridge.isolation] # mode, allowed_builtins, disabled_builtins, minimal_free_model
[workspace.<name>] # path, permission_mode
[[provider]]       # id, bin, models, models_cmd, chat_cmd, bridge, reasoning, etc.
```

**Env Overrides**:
- `AGENTGATE_BRIDGE_PORT` — bridge port
- `AGENTGATE_BRIDGE_CALL_TTL_SECS` / `AGENTGATE_BRIDGE_PARK_TTL_SECS` — TTLs
- `AGENTGATE_BRIDGE_MAX_PARKED_RUNS` — max parked runs
- `AGENTGATE_BRIDGE_TRANSPORT` — `http` or `stdio_shim`
- `AGENTGATE_PROVIDER_TIMEOUT_SECS` — provider call timeout (default 120s)
- `AGENTGATE_STREAM_IDLE_TIMEOUT_SECS` — stream idle timeout (default 120s)

---

## Critical Issues & Gaps

### P1 — Hardcoded Tool Injection Limit (INVEST-TOOL-FILTER)

**File**: `src/provider.rs` lines 1480, 1697
```rust
let max_inject = if is_free_model { 10 } else { 128 };
```
**Problem**: Free models (`beam-free`, `iceman-free`, etc.) are capped at 10 injected tools. Clients sending >10 tools (common with AionUI's 12+ tools) will have tools silently dropped without warning to the client.

**Impact**: Tool calls to dropped tools will fail with `tool_isolation_unavailable` because the tool isn't in the session whitelist.

**Recommendation**: Make this configurable via `[bridge.limits] max_inject_free` / `max_inject_paid` in `agentgate.toml` rather than hardcoding.

### P2 — Placeholder / Stub Implementations

| Location | Stub | Impact |
|----------|------|--------|
| `bridge.rs:367-373` | `init_batcher` handler just logs, doesn't forward | Batcher is initialized but does nothing useful |
| `bridge.rs:746` | `initialize_schemas` returns `Ok(())` with 0 schemas | Tool schemas never initialized from provider registry |
| `bridge.rs:1321` | `generate_tool_schemas` returns empty `Vec` | No agentgate-native tools available |
| `bridge.rs:1085` | `probe_cli_tools_live` returns `Ok(vec![])` | Live CLI tool probing not implemented |
| `bridge.rs:133-142` | `handle_tool_call` / `handle_user_message` are TODO | BridgeClient methods are no-ops |
| `run.rs:410` | `PromptBuilder::with_custom_instructions` is no-op | Custom instructions not supported |
| `run.rs:451` | `RequestBatcher` in run.rs is unused duplicate | Dead code; bridge.rs has its own batcher |

### P3 — BridgeCliProvider `check_available` Inconsistency

**File**: `src/provider.rs` lines 1418-1437

`BridgeCliProvider::check_available` spawns `bin --version` directly without `resolve_binary()`, unlike `CliProviderImpl::check_available` which uses `resolve_binary()` first. On Windows with `.cmd`/`.bat` shims (npm/nvm), this will fail.

**Fix**: Use `self.resolve_binary(bin)` like `CliProviderImpl` does.

### P4 — `minimal_free_model` Skips Permission Blocks

**File**: `src/config.rs` line 244, `src/provider/bridge/opencode.rs` lines 721-725

When `minimal_free_model = true` (default), the `permission` block is omitted from `opencode.json` for free models. This means free models run with **no permission restrictions** — only the `tools` block is applied. If the tools block is also minimal (free model path uses `build_minimal_mcp_config` which omits both), the CLI may have access to all built-in tools.

**Current mitigation**: `build_minimal_mcp_config` (line 735) only injects MCP connection config, no tools/permission at all. This is intentional to avoid `provider.free-limit`, but means **free models have no tool isolation**.

### P5 — Dead Code / Unused Imports

Several items marked `#[allow(dead_code)]` or never called:
- `bridge.rs`: `cleanup_interval`, `batcher` (batcher is initialized but handler is no-op)
- `bridge.rs`: `BridgeSession` struct (unused)
- `bridge.rs`: `BatchedRequest` enum (unused)
- `run.rs`: `RequestBatcher` struct + `BatchedRequest` (duplicate of bridge.rs)
- `run.rs`: `PromptBuilder` (never instantiated in production)
- `run.rs`: `ProbeCache` (preflight_probe is method on RunRegistry, never called from API)

---

## Security Assessment

| Check | Status | Notes |
|-------|--------|-------|
| Token validation | ✅ Constant-time compare | `BridgeServer::validate_token` |
| Browser origin rejection | ✅ Bridge endpoints reject `Origin` header | `api.rs:871`, `api.rs:1041` |
| API key auth | ⚠️ Config present but not enforced | `server.api_keys` checked in config but no middleware |
| Tool whitelist enforcement | ✅ Fail-closed | `tools/call` rejects tools not in session |
| Isolation preflight | ✅ Before every tool-capable turn | `preflight_isolation_check` + `preflight_active_probe` |
| Workdir isolation | ✅ Empty session workdir | `std::env::temp_dir().join("agentgate/sessions/{id}/work")` |
| Process cleanup | ✅ `kill_on_drop(true)` + `taskkill /T` on Windows | Orphan PID sweep on startup |
| CORS | ⚠️ Disabled by default | `server.cors = false` |

---

## Performance Assessment

| Area | Status | Notes |
|------|--------|-------|
| Model cache | ✅ 300s TTL + stale fallback | `warm_model_cache()` at startup |
| Parallel model scan | ✅ 10s timeout per provider | `tokio::spawn` + `join_all` |
| Concurrency control | ✅ Per-provider semaphores | Default 2 concurrent per provider |
| Streaming TTFB | ✅ Early role chunk + 120s first-token timeout | `early_role_chunk = true` |
| Delta coalescing | ✅ 40ms / 256 byte flush | `Coalescer` |
| Workdir cleanup | ✅ On session close + startup sweep | `sweep_orphan_session_dirs` |

---

## Quota Efficiency Analysis

### Free Model Handling

AgentGate has extensive free-model handling to avoid `provider.free-limit` (HTTP 429):

1. **Minimal config injection**: `build_minimal_mcp_config` skips `tools`/`permission` blocks entirely
2. **Tool truncation**: Max 10 tools for free models, 128 for paid
3. **No permission block**: `minimal_free_model = true` skips verbose permission config

### Recommendations for Quota Efficiency

1. **Make tool limit configurable** (P1): Currently hardcoded at 10/128. Add to config:
   ```toml
   [bridge.limits]
   max_inject_free = 10
   max_inject_paid = 128
   ```

2. **Tool selection strategy**: Instead of truncating to first N tools, prioritize by:
   - Client's `tool_choice` if specified
   - Most recently used tools
   - Essential tools (read, write, bash) over specialized ones

3. **Cache model scans more aggressively**: Current 300s TTL is reasonable but could be extended for free models that change less frequently.

4. **Batch tool calls**: The batcher infrastructure exists but is a no-op. Implementing actual batching could reduce round-trips.

---

## Build & Test Commands

```bash
# Build
cargo build --release

# Run server
cargo run -- serve

# CLI commands
cargo run -- init              # Generate default config
cargo run -- providers         # List providers + availability
cargo run -- models            # List all models (with cache)
cargo run -- test <model> <prompt>
cargo run -- doctor            # Health check
cargo run -- spike-bridge      # Integration test bridge

# Run tests
cargo test
```

---

## Dependency Summary

| Crate | Purpose |
|-------|---------|
| `axum` | HTTP server |
| `tokio` | Async runtime |
| `serde` / `toml` | Config serialization |
| `anyhow` / `thiserror` | Error handling |
| `tracing` | Structured logging |
| `uuid` | Session/run IDs |
| `async-trait` | Async trait methods |
| `reqwest` | Ollama HTTP native |
| `rmcp` | MCP SDK |
| `schemars` | JSON schema generation |
| `clap` | CLI argument parsing |
| `which` | Binary resolution |
| `strip-ansi-escapes` | ANSI code stripping |
| `futures` | Stream combinators |
| `chrono` | Timestamps |
| `openai-api-types` | OpenAI type definitions |

---

## Summary

**Overall Status**: Well-architected, production-quality gateway with advanced MCP bridge, tool isolation, and multi-CLI support.

**Strengths**:
- Clean separation: HTTP gateway ↔ provider abstraction ↔ bridge
- Fail-closed design (timeouts, isolation preflight, availability gates)
- Robust Windows binary resolution (nvm, npm, PATHEXT)
- Streaming parity: native CLI + bridge both implement `send_chat_completion_stream`
- Comprehensive free-model handling (minimal config, tool limits)
- Good observability via structured tracing + Prometheus metrics

**Priority Fixes**:
1. **P1**: Make tool injection limit configurable (currently hardcoded 10/128)
2. **P2**: Implement or remove placeholder batcher/schema initialization
3. **P3**: Fix `BridgeCliProvider::check_available` to use `resolve_binary`
4. **P4**: Document that `minimal_free_model` disables permission blocks (security trade-off)
5. **P5**: Clean up dead code (unused batcher, PromptBuilder, ProbeCache)

**Watch Items**:
- Bridge session lifecycle under load (parked runs, continuation cleanup)
- Free model tool injection limit (10 tools) — may need dynamic config
- Ollama native tool calling gap (chat-only, no tool support)
- First-request latency without cache warm (mitigated by `warm_model_cache`)