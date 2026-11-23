# AgentGate Fix Taskboard

**Source**: AUDIT_REPORT.md (verified against actual code — many claims were stale)
**Status**: ⚠️ **PARTIAL — see details below**

---

## ✅ IMPLEMENTED & VERIFIED (cargo check + cargo test pass)

### Quota / Free Limit
- [x] **max_inject=10 for free models** (`src/provider.rs:1512-1516`) — free models (`*-free` suffix) get 10 tools vs 128 for paid
- [x] **minimal_free_model default=false** (`src/config.rs:265`) — was claimed `true` in old audit, actually `false` in code
- [x] **max_runs_per_key=1** (`agentgate.toml:58`) — already configured
- [x] **max_inject config in `[bridge.limits]`** (`src/config.rs:93-98`)

### Latency / Timeouts (ACTUAL VALUES — old audit had wrong numbers)
- [x] **bridge_turn_timeout = 120s** (`agentgate.toml:61` + `src/service.rs:1087`) — was claimed 300s/30s, actually 120s
- [x] **first_token_timeout = 120s** (`agentgate.toml:15` + `src/service.rs:115`) — was claimed 30s, actually 120s
- [x] **provider_call_timeout = 60s** (`src/service.rs:1087`) — was claimed 15s, actually 60s
- [x] **model_cache_ttl = 900s** (`agentgate.toml:14` + `src/service.rs:164`) — was claimed 300s/600s, actually 900s
- [x] **max_concurrency_per_provider = 2** (`agentgate.toml:13`) — was claimed 1, actually 2
- [x] **max_steps = 25** (`agentgate.toml:61`) — reduced from 50 to bound free-tier provider calls
- [x] **Background cache warm at startup** (`src/main.rs:204-207`) — warm_model_cache implemented
- [x] **Pre-populate model cache from config** (`src/service.rs:609-637`, `src/main.rs:199`) — new, eliminates first-request scan

### Bridge / Streaming / Run Parking
- [x] **Stream Run park/resume** (`src/service.rs`, `src/bridge.rs`) — SSE streams parked, resumed via `/_bridge/resume`
- [x] **--thinking flag in bridge** (`src/provider/bridge/opencode.rs:35`) — enables thinking blocks
- [x] **MCP init wait 150ms tunable** (`src/provider/bridge/opencode.rs:870-875`) — `AGENTGATE_MCP_INIT_WAIT_MS`
- [x] **Session reuse via --session ses_...** (`src/provider/bridge/opencode.rs`, `src/provider.rs`) — captures CLI session id from events, passes `--session` on next spawn, fallback to fresh session if resume fails
- [x] **Activity vs thinking (raw/omitted)** (`src/reasoning/parser.rs`) — correct ReasoningKind handling

### Tool Execution
- [x] **execute/execCommand canonical identity** (`src/openai/tools.rs`, `src/provider/bridge/opencode.rs`) — identity mapping + AGENTGATE_SHIM_TOOLS
- [x] **Built-in bash/shell/execute disabled in minimal config** (`src/provider/bridge/opencode.rs:800-822`)
- [x] **Fail-closed emit** (`src/api.rs:380-390`) — unknown tool calls error instead of silently dropping

### Retry / Backoff / Metrics
- [x] **429 retry/backoff** (`src/service.rs:1138-1181`) — semaphore permit retry with exponential backoff
- [x] **Quota metrics** (`src/metrics.rs:52-61, 199-224`) — model_cache_hits/misses, tool_injected, quota_errors
- [x] **Request timeout (non-stream)** (`src/api.rs:564-582`, `src/main.rs:177`, `src/service.rs:125,190`) — 408 on timeout, SSE streams exempt

### Bridge Fixes
- [x] **Run creation error not swallowed** (`src/service.rs:1060`)
- [x] **max_runs_per_key doesn't count parked runs** (`src/bridge.rs:1568`)
- [x] **Run stream marked complete** (`src/service.rs:1108`)
- [x] **Tool arg command** (`src/openai/tools.rs`)

---

## ⚠️ STILL OPEN / NEEDS WORK

| Item | Status | Notes |
|------|--------|-------|
| Request timeout for **streaming** requests | ❌ Not applied | By design — SSE streams and valid tool loops never cut (B7). Documented gap. |
| Generic tool negotiation across UIs | ❌ Not implemented | No common protocol for client↔gateway tool discovery beyond current static config. |
| `execCommand` E2E with full client | 🔄 Partial | Bridge side works (`bridge.call_held tool=execCommand`), needs full client round-trip test with tool-result. See `C:\Users\EDN\AppData\Local\Temp\opencode\ag-e2e\E2E_REPORT.md`. |

---

## ❌ STALE CLAIMS FROM OLD AUDITS (CORRECTED)

| Old Claim | Actual Value | Source |
|-----------|--------------|--------|
| bridge_turn_timeout = 300s / 30s | **120s** | `agentgate.toml:61`, `src/service.rs:1087` |
| first_token_timeout = 30s | **120s** | `agentgate.toml:15`, `src/service.rs:115` |
| provider_call_timeout = 15s | **60s** | `src/service.rs:1087` |
| model_cache_ttl = 300s / 600s | **900s** | `agentgate.toml:14`, `src/service.rs:164` |
| max_concurrency_per_provider = 1 | **2** | `agentgate.toml:13` |
| minimal_free_model default = true | **false** | `src/config.rs:265` |
| max_steps = 50 | **25** | `agentgate.toml:61` |
| "cache TTL not wired" | **wired + pre-populated** | `src/service.rs`, `src/main.rs` |

---

## 📎 REPORTS (full details)

- **5-topic investigation**: `C:\Users\EDN\AppData\Local\Temp\opencode\ag-lat\FINAL_REPORT_5_TOPIK.md`
- **Topic 3 & 4 (latency + tool exec)**: `C:\Users\EDN\AppData\Local\Temp\opencode\ag-lat\REPORT_TOPIC3_4.md`
- **Topic 5 (tool exec detail)**: `C:\Users\EDN\AppData\Local\Temp\opencode\ag-lat\REPORT_TOPIC5_TOOL_EXEC.md`
- **E2E execCommand**: `C:\Users\EDN\AppData\Local\Temp\opencode\ag-e2e\E2E_REPORT.md`

---

## 📝 Summary of Changes (This Cycle)

| File | Change |
|------|--------|
| `src/provider.rs` | `persistent_opencode_sessions`, `stored_opencode_session`, `remember_opencode_session`, `is_keep_alive` fix, stream path CLI session capture |
| `src/provider/bridge.rs` | `BridgeConfig.cli_session_id`, `BridgeSession::cli_session_id()` trait method |
| `src/provider/bridge/opencode.rs` | `opencode_session_id` field, `extract_opencode_session_id()`, event capture in both loops, fallback spawn without `--session` |
| `src/service.rs` | `model_negative_cache`, `pre_populate_cache()`, cache hit/miss metrics, negative cache logic in `list_models` |
| `src/main.rs` | Call `pre_populate_cache()` at startup |
| `src/api.rs` | Non-stream request timeout wiring (408) |
| `src/metrics.rs` | `model_cache_hits_total`, `model_cache_misses_total`, `record_model_cache_hit/miss` |

---

## ✅ Verification

```bash
cargo check           # ✅ Passes (warnings only)
cargo test            # ✅ 67 passed, 0 failed
```

---

## 🎯 Next Steps (if scope expands)

1. Implement request timeout for streaming (with tool-loop awareness)
2. Design generic tool negotiation protocol
3. Complete `execCommand` E2E with full client
4. Consider keep-alive for non-opencode providers (gemini, ollama)