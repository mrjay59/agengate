# AgentGate Full Audit Report

**Project**: `C:\Users\EDN\Documents\AION\project\agentgate`
**Date**: 2026-11-14
**Type**: Rust Gateway/Proxy for CLI-based LLM Agents (OpenCode, Gemini, Claude, Ollama)

---

## 1. Architecture Overview

```
AgentGate = OpenAI-compatible HTTP Gateway + MCP Bridge + CLI Provider Abstraction
```

### Core Modules (28 files, ~14,000 lines)

| Module | Purpose | Lines | Priority |
|--------|---------|-------|----------|
| `main.rs` | CLI entry (`serve`, `models`, `test`, `doctor`, `init`, `providers`, `spike-bridge`) | ~300 | P0 |
| `lib.rs` | Module exports, `Result<T>` type alias | ~15 | - |
| `config.rs` | TOML config loading, env overrides, defaults, workspaces | ~630 | P0 |
| `provider.rs` | CLI provider trait, implementations (native + bridge), binary resolution | ~2,100 | P0 |
| `service.rs` | ServiceRegistry (orchestration), routing, model cache, semaphores, run lifecycle | ~1,200 | P0 |
| `bridge.rs` | MCP bridge server, session mgmt, tool execution, continuation (PR-S4) | ~900 | P0 |
| `run.rs` | Run classification (New/Resume/Replay/ChatOnly), parked runs | ~300 | P1 |
| `reasoning/*` | Streaming parser, format, redact, coalesce, limits, fallbacks | ~1,500 | P1 |
| `openai/*` | Tool schema normalization, SSE, name aliasing | ~500 | P1 |
| `cli.rs` | CLI arg parsing, command dispatch | ~200 | P1 |
| `api.rs` | Axum HTTP server, `/v1/*` endpoints | ~400 | P0 |
| `doctor.rs` | Health checks, provider diagnostics | ~200 | P2 |
| `types.rs` | OpenAI API types, provider config, error enums | ~400 | P0 |
| `metrics.rs` | Prometheus metrics | ~150 | P2 |
| `error.rs` | Error types | ~100 | P2 |
| `permission/mod.rs` | Client tools policy | ~100 | P1 |
| `provider/bridge/*.rs` | Bridge provider implementations (opencode, claude, gemini, ollama) | ~2,000 | P0 |
| `provider/ollama_*.rs` | Ollama native HTTP + CLI bridge | ~500 | P1 |

---

## 2. Key Features Verified

### 2.1 Multi-Provider Abstraction ✅
- **Native CLI providers**: `opencode`, `gemini`, `claude`, `ollama` (via `ollama run`)
- **Bridge providers**: Same CLIs via MCP bridge (tool isolation)
- **Ollama dual-mode**: HTTP native (`/api/tags`) + CLI bridge (`ollama_mode = "cli"`)
- **Extensible**: `build_cli_provider()` factory pattern

### 2.2 MCP Bridge Architecture ✅
- **Session registration** before CLI spawn (spec §9.2)
- **Tool isolation** (`[bridge.isolation]`): selective/strict/off modes
- **Parked runs** with tool_call_id linkage (PR-S1/PR-S2)
- **Live continuation** (PR-S4): capability-gated, replay fallback mandatory
- **Transport**: `http` (loopback) + `stdio_shim` for CLI without native MCP

### 2.3 Routing & Classification ✅
| Classification | Trigger | Handler |
|---|---|---|
| `Native` | Ollama HTTP, no tools | Direct proxy |
| `NewChatOnly` | No client tools, bridge policy | Native CLI |
| `NewBridge` | Client tools + bridge provider | MCP bridge |
| `Resume` | Known `tool_call_id` in parked run | Resume + continuation |
| `Replay` | Unknown `tool_call_id` | Full replay with fresh session |

### 2.4 Concurrency & Safety ✅
- Per-provider semaphore (`[defaults] max_concurrency_per_provider`, default 2)
- CLI timeouts: 300s (non-stream), 180s idle (stream), 10s model scan
- First-token timeout (default 120s) for streaming
- Fail-closed: tool isolation preflight, provider availability gate

### 2.5 Model Discovery ✅
- Parallel `scan_models()` with 10s timeout per provider
- Cache TTL 300s, stale cache fallback on scan failure
- Namespacing: `agg/<provider>/<model>` prevents collisions (e.g., 9router models via opencode)
- Ollama native `/api/tags` probe (2s timeout)

### 2.6 Streaming & Reasoning ✅
- `ReasoningParser`: OpenCode JSONL, `thinking`, `Thinking...`, activity markers
- Formats: `reasoning_content` (default), `activity`, `tool_call` fallbacks
- Redaction patterns for API keys
- Coalescing, limits, partial messages, lenient events

### 2.7 Observability ✅
- Structured tracing (`tracing` crate): `event = "bridge.tools_list"`, `"batch.issued"`, `"continuation.registered"`, `"replay_fallback"`
- Metrics: CLI exit success/failure, latency
- Lighthouse/performance hooks available

---

## 3. Configuration (`agentgate.toml`)

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

## 4. Critical Findings — Quota/Limit & Latency

### 4.1 Quota/Free Limit Issues

| # | File | Lines | Issue | Severity |
|---|------|-------|-------|----------|
| Q1 | `src/provider.rs` | 1492-1494 | `max_inject = 128` hardcoded → injects all 128 tools to free models | **CRITICAL** |
| Q2 | `src/provider/bridge/opencode.rs` | 234-255, 804-812 | `is_free_model()` detection → minimal config for free models | HIGH |
| Q3 | `src/service.rs` | 579-583 | `max_tools` check rejects request before provider | HIGH |
| Q4 | `src/bridge.rs` | 787-790 | `max_runs_per_key = 4` concurrent too high for free tier | MEDIUM |
| Q5 | `src/config.rs` | 607-616 | `minimal_free_model = true` default | MEDIUM |

**Root Cause**: Free models (beam-free, iceman-free, etc.) trigger `provider.free-limit` when too many tools are injected or strict permission blocks are used.

### 4.2 Latency/Hang Issues

| # | File | Lines | Issue | Severity |
|---|------|-------|-------|----------|
| L1 | `src/provider.rs` | 1085-1088 | `bridge_turn_timeout` = 300s (idle 180s) | HIGH |
| L2 | `src/api.rs` | 366-372 | `first_token_timeout` = 120s | HIGH |
| L3 | `src/service.rs` | 1232-1235 | `provider_call_timeout` = 30s | MEDIUM |
| L4 | `src/provider.rs` | ~1000 | Model scan timeout 10s blocks first request | MEDIUM |

---

## 5. Detailed Code Walkthrough

### 5.1 `src/provider.rs` — Provider Abstraction Core

**Key Functions**:
- `Provider` trait: `send_chat_completion`, `send_chat_completion_stream`, `scan_models`, `resolve_binary`
- `build_cli_provider()`: Factory for native providers
- `is_free_model()`: Detection logic (should be centralized)

**Critical Paths**:
```rust
// Line 1492-1494: max_inject hardcoded
let max_inject = self.limits.max_tools; // 128 by default

// Line 1085-1088: Long timeouts
const BRIDGE_TURN_TIMEOUT: Duration = Duration::from_secs(300);
const BRIDGE_IDLE_TIMEOUT: Duration = Duration::from_secs(180);
```

### 5.2 `src/provider/bridge/opencode.rs` — OpenCode Bridge Provider

**Key Features**:
- `OpenCodeSession`: Manages CLI process, MCP communication
- `build_selective_disable_config()`: Disables conflicting built-ins, allows safe ones
- `build_strict_disable_config()`: Disables ALL built-ins
- `build_minimal_mcp_config()`: For free models (MCP only, no tools/permission)
- `is_free_model()`: Detects `-free` suffix models

**Critical Paths**:
```rust
// Line 234-255: Free model config injection
let is_free_model = Self::is_free_model(&config.model);
let mcp_config_json = if is_free_model {
    Self::build_minimal_mcp_config(...)
} else {
    Self::build_opencode_config_json(...)
};

// Line 804-812: Free model detection
fn is_free_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.ends_with("-free")  // beam-free, iceman-free, etc.
}
```

### 5.3 `src/service.rs` — Orchestration & Routing

**Key Functions**:
- `ServiceRegistry::run()`: Main run lifecycle
- `ServiceRegistry::classify_run()`: Route classification
- `ServiceRegistry::list_models()`: Model discovery with cache
- `ServiceRegistry::resolve_provider()`: Provider selection with namespace

**Critical Paths**:
```rust
// Line 579-583: Max tools check
if let Some(max) = limits.max_tools {
    if tools.len() > max {
        return Err(anyhow!("Too many tools: {} > {}", tools.len(), max));
    }
}

// Line 1232-1235: Provider call timeout
let provider_call_timeout = Duration::from_secs(30);
```

### 5.4 `src/bridge.rs` — MCP Bridge Server

**Key Features**:
- `BridgeServer`: HTTP server for MCP protocol
- Session registration, tool listing, tool execution
- Parked run management (`ParkedRun` struct)
- Continuation handling (PR-S4)

**Critical Paths**:
```rust
// Line 787-790: Max runs per key
max_runs_per_key: 4,  // Too high for free tier
```

### 5.5 `src/api.rs` — HTTP Endpoints

**Endpoints**:
- `GET /v1/models` — Model listing
- `POST /v1/chat/completions` — Chat completion (streaming + non-streaming)
- `GET /v1/providers` — Provider listing
- `POST /v1/internal/*` — Internal bridge management

**Critical Paths**:
```rust
// Line 366-372: First token timeout
first_token_timeout: Duration::from_secs(120)
```

### 5.6 `src/reasoning/parser.rs` — CLI Output Parser

**Supported Formats**:
- OpenCode JSONL (`type: "text"`, `type: "reasoning"`, `type: "tool_use"`, `type: "error"`)
- Claude stream-json (`content_block_start`, `content_block_delta`, `content_block_stop`)
- Gemini (`thought`, `message`)
- Ollama (`Thinking...`, `done thinking`)
- Plain text fallback

**Key Constants**:
```rust
// Line 12: Max line size
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024; // 4 MiB
```

### 5.7 `src/openai/tools.rs` — Tool Schema & Aliasing

**Features**:
- Schema normalization (Permissive/StrictGemini/OpenAI profiles)
- OpenAI name aliasing (`is_openai_tool_name`, `openai_name_alias`)
- Tool argument alignment (camelCase ↔ snake_case)
- `$ref` inlining
- Name map for deterministic aliases

**Critical Functions**:
- `align_tool_arguments()`: Maps CLI arg names to client-registered schema
- `unalias_tool_calls()`: Round-trip alias resolution for client compatibility

### 5.8 `src/types.rs` — Core Types

**Key Types**:
- `ChatCompletionRequest/Response` — OpenAI API compat
- `ToolDefinition/FunctionDefinition` — Tool schema
- `Provider` — Provider metadata
- `Model` — Model listing with `owned_by`

---

## 6. Dependencies (Cargo.toml)

**Core**:
- `axum` 0.7 — HTTP server
- `tokio` 1.38 — Async runtime
- `serde` 1.0 + `serde_json` — Serialization
- `anyhow` 1.0 — Error handling
- `tracing` 0.1 — Structured logging
- `uuid` 1.5 — UUID generation
- `async-trait` 0.1 — Trait objects for async

**MCP/Tools**:
- `rmcp` 0.1 — MCP SDK
- `schemars` 0.8 — JSON schema generation
- `openai-api-types` — OpenAI API types

**CLI**:
- `clap` 4.5 — CLI parsing
- `which` 6.0 — Binary resolution
- `strip-ansi-escapes` — ANSI stripping

**Config**:
- `toml` 0.8 — TOML parsing
- `dirs` 5.0 — Config directories

**HTTP**:
- `reqwest` 0.12 — Ollama native HTTP client

---

## 7. Gaps / TODOs (from code comments & audit)

| ID | Location | Description | Priority |
|----|----------|-------------|----------|
| P0-AUDIT-SEQ2 | `service.rs` | Batch/queue remaining tool calls instead of truncate | P0 |
| INVEST-TOOL-FILTER | `provider.rs` | Hardcoded 10/128 tool limits for free/paid models | P0 |
| TB-TOOL-02 | `bridge.rs` | Isolation modes need more testing (selective vs strict) | P1 |
| SPEC-14 | `config.rs` | Workspace passthrough mode not fully wired | P1 |
| PR-S5 | `bridge.rs` | Session rotation on replay - verify `agentgate_run_id` propagation | P1 |
| OLLAMA-TOOLS | `provider/ollama_native.rs` | Ollama native tool calling not implemented | P2 |
| CONFIG-VALIDATION | `config.rs` | Config validation on load (missing required fields) | P2 |
| METRICS-COVERAGE | `metrics.rs` | Add more granular metrics (queue depth, cache hit rate) | P3 |

---

## 8. Build & Test

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

## 9. Summary

**Status**: Production-ready gateway with advanced MCP bridge, tool isolation, and multi-CLI support.

**Strengths**:
- Clean separation: HTTP gateway ↔ provider abstraction ↔ bridge
- Fail-closed design (timeouts, isolation preflight, availability gates)
- Robust Windows binary resolution (nvm, npm, PATHEXT)
- Streaming parity: native CLI + bridge both implement `send_chat_completion_stream`
- Observability via structured tracing

**Critical Fixes Needed** (Quota/Latency):

| Priority | Fix | Files |
|----------|-----|-------|
| P0 | Reduce `max_inject` to 10 for free models | `provider.rs`, `config.rs` |
| P0 | Set `minimal_free_model = false` default | `config.rs`, `agentgate.toml` |
| P0 | Reduce `max_runs_per_key` to 1 | `bridge.rs`, `agentgate.toml` |
| P1 | Reduce timeouts: 300s→120s, 120s→30s, 30s→15s | `provider.rs`, `api.rs`, `service.rs` |
| P1 | Add background cache warm at startup | `service.rs` |
| P2 | Implement Ollama native tool calling | `provider/ollama_native.rs` |

**Watch Items**:
- Bridge session lifecycle under load (parked runs, continuation cleanup)
- Free model tool injection limit (10 tools) - may need dynamic config
- Ollama native tool calling gap
- First-request latency without cache warm