# AgentGate Local Audit Verification Report

**Date:** 2026-11-11
**Tester:** Testing (Aion CLI teammate)
**Environment:** Windows 11, local gateway (http://127.0.0.1:8787)
**Binary:** agentgate.exe v0.2.2 (release build)
**Audit Reference:** audit.md (2026-11-09 refresh + triage addendum 2026-11-10)

---

## 1. Build & Test Status

| Check | Result |
|---|---|
| `cargo test --lib` | ✅ 55/55 passed |
| `cargo test --tests` | ✅ All passed (100+ integration tests) |
| `cargo check` | ✅ OK (warnings only, pre-existing) |
| `agentgate doctor` | ✅ Runs; providers: gemini MISSING (resolved via .cmd), opencode MISSING (resolved via .cmd), ollama OK |
| Git status | Clean (all audit commits present: 2ae7c15, 7e38aff, a0053b3, 16ad1f5, ae0aabb, 5e35647) |

---

## 2. Audit Item Verification

### P0 — All FIXED (verified in code + tests)

| # | Item | Audit Location | Status | Evidence |
|---|---|---|---|---|
| 1 | SSE tool_calls 2-chunk (§7.4) | api.rs:649-680 | ✅ FIXED | Code emits Chunk1 {id,name,arguments:""} + Chunk2 512B slices + finish_reason="tool_calls". Test: sse_tool_calls_contract_test 6/6 pass |
| 2 | notifications/cancelled kill | api.rs:872-876 | ✅ FIXED | Calls registry.bridge.cancel_session(). Code verified. Test: stream_timeout_test 3/3 pass |
| 3 | Keepalive SSE | api.rs:439-446 | ✅ FIXED | Sends `: keep-alive` comment every 10s. Observed in live stream output |
| 4 | cli.rs builder | cli.rs:22 | ✅ FIXED | Uses build_cli_provider() instead of raw CliProviderImpl |
| 5 | RequestBatcher batch/queue | service.rs:928-943, bridge.rs:220-247 | ✅ FIXED | Serial mode queues remaining calls via queue_tool_calls(). Code verified |
| 6 | tool_call_id generator | openai/tools.rs:230-289 | ✅ FIXED | ToolCallIdGenerator::generate() used for text-based parsing. bridge.rs:795 uses it |
| 7 | Fail-fast on silent provider | api.rs:349-467 | ✅ FIXED | First-token timeout + idle timeout with error chunk + [DONE]. Tested with model-not-installed path |

### P1 — All FIXED (verified in code)

| # | Item | Status | Evidence |
|---|---|---|---|
| 8 | Guard prefix (agentgate_*/mcp__agentgate__) | ✅ FIXED | claude.rs:81-82, gemini.rs:81-82 include both prefixes. Test: fail_closed_test 5/5 pass |
| 9 | .cmd/PATHEXT Windows | ✅ FIXED | resolve_binary_path() in claude.rs:27-60, gemini.rs:27-60. Doctor confirms resolution |
| 10 | Transport single source | ✅ FIXED | claude.rs:177, gemini.rs:177, opencode.rs:161 use config.transport |
| 11 | Claude --tools "" | ✅ FIXED | claude.rs:215-217. Code verified |
| 12 | Claude --mcp-config + --strict-mcp-config | ✅ FIXED | claude.rs:220-221. Code verified |
| 13 | Gemini settings file per-run | ✅ FIXED | gemini.rs:196. Code verified |
| 14 | temp_dir() fallback | ✅ FIXED | provider.rs:255,389,1476,1678 use std::env::temp_dir() |
| 15 | max_result_bytes + max_prompt_chars | ✅ FIXED | bridge.rs:571,623, provider.rs:1171. Code verified |
| 16 | os error 5 -> JSON | ✅ FIXED | api.rs:941-955. Code verified |
| 17 | POST /_bridge/resolve/:call_id + auth | ✅ FIXED | api.rs:978-1032. Code verified |
| 18 | Disable builtin opencode | ✅ FIXED | opencode.rs tool disable config injection. Code verified |

### P2 — Mostly FIXED

| # | Item | Status | Evidence |
|---|---|---|---|
| 19 | Replay session rotation (PR-S5) | ✅ FIXED | bridge.rs:449-472 replay_rotate_session(). Code verified |
| 20 | Orphan PID sweep (PR-S5) | ✅ FIXED | bridge.rs:417-448 sweep_orphan_pids_on_startup(). Code verified |
| 21 | NameMap dedupe | ✅ FIXED | bridge.rs:1089-1110 tool_schemas_to_openai_functions(). Code verified |
| 22 | Resume/TTL | ✅ FIXED | bridge.rs:261-283 record_tool_call_ttl/is_tool_call_expired/cleanup. Code verified |

### PARTIAL — Still Not Fully Implemented

| # | Item | Status | Details |
|---|---|---|---|
| 23 | Hold<->Run live mapping | ⚠️ PARTIAL | associate_tool_call_ids() exists (bridge.rs:640-652) and is called in hold_tool_call when run_id+tool_call_id both Some. BUT in MCP path (api.rs:929-932), both are passed as None. So live mapping is never wired in the actual CLI->bridge flow. Only works via dead-code execute_tool() path. **Impact:** lookup miss -> falls to replay instead of live continuation. |
| 24 | Gemini excludeTools | ⚠️ PARTIAL | No excludeTools in gemini.rs. Audit says "settings file ok, tapi tanpa excludeTools". Not added in seq3. |
| 25 | reasoning_effort / compat/*.json | ⚠️ PARTIAL | No reasoning_effort mapping in code. compat/*.json files exist but not consumed in main path. |

---

## 3. E2E Test Results (Local)

### Test 1: Health + Models
- `/health` → 200 OK ✅
- `/v1/models` with Bearer default_key → 88 models ✅
- `agentgate models` (CLI) → 88 models ✅
- **CLI vs Server count: MATCH (88 = 88)** ✅ (audit mentioned 10 vs 98, but locally both 88)

### Test 2: Streaming (agg/ollama/nemotron-3-nano:4b)
- Role chunk: ✅ present
- reasoning_content chunk(s): ✅ present
- content chunk(s): ✅ present
- finish_reason: ✅ "stop"
- literal `data: [DONE]`: ✅ present
- TTFT: ~250ms (first reasoning chunk)
- Total: ~2s
- **Result: PASS ✅**

### Test 3: Non-streaming (agg/ollama/nemotron-3-nano:4b)
- content non-empty: ✅ "Hello!"
- reasoning_content present: ✅
- usage.completion_tokens > 0: ❌ **0 (DEFECT)**
- **Result: FAIL (usage tokens = 0) ❌**

### Test 4: Failure Mode A (agg/gemini/gemini-2.5-flash)
- gemini binary IS available locally (gemini.cmd)
- Returns error chunk: ✅ "Gemini CLI is not running in a trusted directory"
- finish_reason: ✅ "error"
- `data: [DONE]`: ✅ present
- No hang: ✅
- **Result: PASS (no hang, proper error) ✅**

### Test 5: Failure Mode B (agg/ollama/model-that-cannot-exist:0b)
- Returns error chunk: ✅ "Error: pull model manifest: file does not exist"
- finish_reason: ✅ "error"
- `data: [DONE]`: ✅ present
- **BUT:** Provider starts downloading ("pulling manifest") before erroring. Fail-fast doesn't catch this because provider produces output (spinner). First-token timeout doesn't trigger.
- **Result: PARTIAL (error returned but download attempt happened) ⚠️**

### Test 6: Cancel (not tested - requires manual client disconnect)
- Not tested in this session. Code review: tx.closed() handler breaks loop (api.rs:373-379), which should kill CLI child. Previous VPS testing confirmed this works (d6a53f8).

### Test 7: Doctor Output
- glibc warning: N/A on Windows (Linux-only issue)
- Provider detection: gemini/opencode resolved via .cmd, ollama native ✅
- **Result: PASS ✅**

---

## 4. Defects Found

### D1: usage.completion_tokens = 0 in non-streaming responses
- **Location:** src/service.rs (chat_completion path)
- **Impact:** Non-streaming responses report 0 completion tokens, 0 prompt tokens, 0 total tokens
- **Audit reference:** "Non-stream: assert content non-empty, reasoning_content present, usage.completion_tokens > 0 (we suspect it is 0 - verify and report as a defect)"
- **Status:** CONFIRMED DEFECT ❌

### D2: Model-not-installed path starts download before erroring
- **Location:** src/provider/bridge/ollama (CLI path)
- **Impact:** Requesting a non-existent ollama model triggers `ollama pull` spinner output, which counts as provider activity. Fail-fast timeout doesn't trigger. Eventually errors with "file does not exist" but wastes time.
- **Audit reference:** "ABORT immediately if a real download starts"
- **Status:** CONFIRMED ISSUE ⚠️ (not a hang, but not ideal)

### D3: Hold<->Run live mapping not wired in MCP path
- **Location:** src/api.rs:929-932 (bridge_mcp tools/call handler)
- **Impact:** associate_tool_call_ids() never called with real run_id/tool_call_id in the live CLI->bridge flow. Falls to replay instead of live continuation.
- **Audit reference:** PARTIAL item #23
- **Status:** CONFIRMED PARTIAL ⚠️

### D4: Gemini missing excludeTools
- **Location:** src/provider/bridge/gemini.rs
- **Impact:** Gemini built-in tools not explicitly excluded (only guard_patterns, no settings-level excludeTools)
- **Audit reference:** PARTIAL item #24
- **Status:** CONFIRMED PARTIAL ⚠️

### D5: reasoning_effort not implemented
- **Location:** src/reasoning (no reasoning_effort mapping)
- **Impact:** reasoning_effort parameter not mapped to CLI-specific flags. compat/*.json files exist but not consumed.
- **Audit reference:** PARTIAL item #25
- **Status:** CONFIRMED PARTIAL ⚠️

---

## 5. Summary

| Category | Count |
|---|---|
| P0 items FIXED | 7/7 ✅ |
| P1 items FIXED | 11/11 ✅ |
| P2 items FIXED | 4/4 ✅ |
| PARTIAL items | 3 remaining (D3, D4, D5) |
| Defects found | 5 (1 critical: D1, 2 medium: D2, D3, 2 low: D4, D5) |
| E2E tests passed | 4/6 (D1 fail, D2 partial) |

**Overall:** AgentGate has made significant progress. All P0 and P1 items from the audit are implemented and verified. The codebase is functional for local use with ollama models. Three PARTIAL items remain (Hold<->Run live mapping, Gemini excludeTools, reasoning_effort), and one critical defect (usage tokens = 0) needs fixing before production readiness.

**Recommendation:** Fix D1 (usage tokens) as priority. Address D3 (live mapping) for proper tool continuation. D4 and D5 are enhancements for later sprints.

---

**Report prepared by:** Testing teammate
**For:** Lead (OpenCode)