# EVIDENCE — Local Gateway Audit Verification (audit.md)

**Date:** 2026-11-11
**Tester:** Testing (slot 01a1bfdf-a75d-74e3-9131-a2dc96e41d09)
**Gateway:** http://127.0.0.1:8780 (agentgate.exe serve, PID 26216)
**Config:** agentgate.toml (port 8780, api_keys=["default_key"])
**Model used for E2E:** agg/ollama/nemotron-3-nano:4b (local, zero quota)
**Source audit:** audit.md (2026-11-09, refresh 2026-11-10)

---

## 1. Build / Test Status

| Check | Result |
|---|---|
| `cargo build` (debug) | PASS (warnings only: unused var in main.rs:353) |
| `cargo test --lib` | PASS — 55/55 |
| `cargo test --tests` | PASS — all integration test binaries (bridge_mcp_hold 1, spec_scenarios 8, fail_closed 5, sse_contract, stream_timeout, stage2_continuation, live_resume_e2e 15, schema_normalization 7, etc.) |
| `agentgate doctor` | RUNS — opencode/gemini reported MISSING (cosmetic; .cmd resolves), ollama OK |

---

## 2. Audit.md Item Verification (Code + Local E2E)

### 2.1 P0 (Blocker R1) — All FIXED

| # | Audit Item | Location | Status | Evidence |
|---|---|---|---|---|
| 1 | SSE tool_calls 2-chunk (§7.4) | api.rs:644-680 | ✅ FIXED | Code emits Chunk1 {id,name,arguments:""} + Chunk2+ 512B slices + finish_reason="tool_calls". Contract test `sse_tool_calls_contract` PASS. |
| 2 | notifications/cancelled kill | api.rs:872-877 | ✅ FIXED | Handler calls `bridge.cancel_session()` → kill CLI + cleanup workdir. Integration test `delete_session_wakes_held_calls` PASS. |
| 3 | Keepalive during MCP hold | bridge.rs:526-555 | ✅ FIXED | Biased select! loop with 250ms ticker (event: bridge.tool_call.hold_keepalive). Integration test PASS. |
| 4 | cli.rs uses build_cli_provider | cli.rs:22 | ✅ FIXED | `execute_providers` uses `build_cli_provider()` + native fallback. |

### 2.2 P1 (Isolation & Windows) — Mostly FIXED

| # | Audit Item | Location | Status | Evidence |
|---|---|---|---|---|
| 5 | Guard prefix agentgate_*/mcp__agentgate__ | claude.rs/gemini.rs default_profile | ✅ FIXED | guard_patterns includes all 14 patterns. fail_closed_test (5 tests) PASS. |
| 6 | .cmd handling Windows | resolve_binary_path() | ✅ FIXED | opencode.cmd/gemini.cmd resolve correctly (doctor confirms). |
| 7 | Transport single source | build_*_mcp_config_json | ✅ FIXED | Uses {transport}/{transport_url} from agentgate.toml. |
| 8 | Claude --tools "" | claude.rs spawn() | ✅ FIXED | claude spawn adds `--tools ""`. fail_closed_test PASS. |
| 9 | Gemini excludeTools | gemini.rs settings | ⚠️ PARTIAL | Settings file injected with tools/permissions from tool_disable_config, but no explicit excludeTools. |
| 10 | Fail-closed guard 503 | bridge.rs/api.rs | ✅ FIXED | fail_closed_test (5/5) PASS. |

### 2.3 P2 (Lifecycle) — Mostly FIXED

| # | Audit Item | Location | Status | Evidence |
|---|---|---|---|---|
| 11 | RequestBatcher batch/queue | service.rs:928-964 | ✅ FIXED | truncate_tool_calls replaced with batch/queue. spec_scenarios_test PASS (t5, t20). |
| 12 | tool_call_id generator | run.rs:196-199, openai/tools.rs:232 | ✅ FIXED | ToolCallIdGenerator::new() used. associate_tool_call_ids wired in bridge. |
| 13 | NameMap/limit | bridge.rs:175,248-255 | ✅ FIXED | tool_name_map + register/resolve alias. schema_normalization_test (7 tests) PASS. |
| 14 | Resume/TTL | service.rs:734-844, bridge.rs:261-279 | ✅ FIXED | Parked run resume + TTL tracking. live_resume_e2e_test (15 tests) PASS. |
| 15 | Replay session rotation (PR-S5) | bridge.rs (cli_gen, session_generation) | ✅ FIXED | sweep_orphan_pids, replay_rotate_session. cargo tests PASS. |
| 16 | reasoning_effort + compat/*.json | types.rs:103, config.rs:417 | ⚠️ PARTIAL | Defined in types/config but NOT wired into service/run/provider path. |

### 2.4 Infrastructure/Operational

| Item | Status | Notes |
|---|---|---|
| CI runner build-essential | ✅ CLOSED | CI run 775bdc0 SUCCESS (all jobs green, artifacts verified) |
| glibc target 2.35 | ✅ CLOSED | CI verifies <= GLIBC_2.35 (commit 9af0f60) |
| Target-env E2E (Linux + 9router/AionUI/VSC Copilot) | ⏳ PENDING | Infra pending |
| CI artifacts/checksums | ✅ CLOSED | verify-all-artifacts passed |
| Opencode free usage limit | ⏳ BLOCKED | Quota reset needed for live eval |

---

## 3. Local Gateway E2E Matrix (port 8780)

### 3.1 Health & Models

| Test | Result | Details |
|---|---|---|
| GET /health | ✅ PASS | `{"status":"ok"}` HTTP 200 |
| GET /v1/models (Bearer default_key) | ✅ PASS | 88 models advertised |
| GET /_bridge/mcp/:session (no Bearer) | ✅ PASS | HTTP 401 |
| POST /v1/chat/completions n=2 | ✅ PASS | HTTP 400 (n>1 not supported) |
| agentgate models CLI vs /v1/models | ✅ MATCH | 88 vs 88 |

### 3.2 Streaming (agg/ollama/nemotron-3-nano:4b, 13.3s)

| Assertion | Result | Details |
|---|---|---|
| Role chunk present | ✅ PASS | `delta.role="assistant"` |
| reasoning_content chunk(s) | ✅ PASS | Streamed reasoning text (1 chunk) |
| content chunk(s) | ✅ PASS | "Hello!\n" |
| finish_reason=stop | ✅ PASS | |
| literal `data: [DONE]` | ✅ PASS | Terminal marker present |
| Keep-alive does not replace terminal | ✅ PASS | `: ping` / `: keep-alive` lines present mid-stream but [DONE] still emitted at end |

### 3.3 Non-Stream (agg/ollama/nemotron-3-nano:4b, 9.2s)

| Assertion | Result | Details |
|---|---|---|
| content non-empty | ✅ PASS | "Hello." |
| reasoning_content present | ✅ PASS | Present |
| usage.completion_tokens > 0 | ❌ FAIL (DEFECT) | completion_tokens=0, prompt_tokens=0, total_tokens=0. Usage accounting not populated for ollama bridge path. |

### 3.4 Cancel Test

| Assertion | Result | Details |
|---|---|---|
| Start stream, kill client after 3s | ✅ PASS | No orphan "ollama run" processes found within 10s. Only ollama serve + ollama app (main processes). Server kill-on-disconnect working (commit d6a53f8). |

### 3.5 Failure Mode A (agg/gemini/gemini-2.5-flash)

| Assertion | Result | Details |
|---|---|---|
| Elapsed time | 1.96s | Fast (not hang) |
| Error chunk + [DONE] | ✅ PASS | finish_reason=error, code=provider_error |
| Note | gemini.cmd present but fails (not trusted directory) | |

### 3.6 Failure Mode B (agg/ollama/definitely-not-a-model:0b)

| Assertion | Result | Details |
|---|---|---|
| Elapsed time | 1.86s | |
| Error chunk + [DONE] | ✅ PASS | finish_reason=error |
| ⚠️ Risk | ollama attempted "pulling manifest" before failing | No real download completed (model doesn't exist on registry), but ollama CLI starts pull process. Risky for real non-installed models. |

### 3.7 agentgate doctor

| Provider | Status | Notes |
|---|---|---|
| ollama | OK | Available, 3 models |
| opencode | MISSING (cosmetic) | .cmd resolves (opencode.cmd found on PATH). Doctor first-check fails but resolve succeeds. |
| gemini | MISSING (cosmetic) | Same as above (gemini.cmd found). |

---

## 4. Summary of Defects Found

1. **usage.completion_tokens == 0 on non-stream** (DEFECT, HIGH) — ollama bridge path does not populate usage tokens. Confirmed in local E2E. Needs fix in provider/ollama path.
2. **reasoning_effort not wired** (PARTIAL, LOW) — types/config defined but not consumed in service/run/provider.
3. **Gemini excludeTools not implemented** (PARTIAL, MEDIUM) — settings file ok, but no excludeTools. Claude has --tools "" but Gemini relies on config injection only.
4. **doctor reports opencode/gemini as MISSING** (cosmetic, LOW) — .cmd resolves but availability flag not updated.
5. **New test files not created** (checklist items, MEDIUM) — cancel_abort_test.rs, claude_strict_mcp_test.rs, sse_tool_calls_two_chunk_test.rs are listed in audit.md checklist but not present in tests/.
6. **Failure mode B: ollama attempts pull manifest** (MEDIUM) — for non-existent models, ollama CLI starts "pulling manifest" before failing. For real non-installed models, this could trigger multi-GB download. Consider pre-checking model existence.

---

## 5. Overall Verdict

- **Core bridge (P0):** 4/4 FIXED and verified (code + tests + local E2E).
- **Isolation (P1):** 4/5 FIXED (Gemini excludeTools pending).
- **Lifecycle (P2):** 5/6 FIXED (reasoning_effort pending).
- **Infrastructure:** CI GREEN (775bdc0), glibc 2.35 target, artifacts verified.
- **Local gateway E2E:** Streaming PASS (all assertions), non-stream PASS (except usage tokens defect), cancel PASS, failure modes A&B fast error.
- **Overall compliance with audit.md:** ~90% (reasoning_effort + usage tokens + Gemini excludeTools + target-env E2E remain).

---

## 6. Recommendation

1. Fix usage token accounting in ollama provider path (non-stream) — HIGH priority.
2. Add Gemini excludeTools or equivalent builtin disable — MEDIUM.
3. Wire reasoning_effort into service/run/provider — LOW priority.
4. Fix doctor availability flag for .cmd-resolved providers — LOW.
5. Create missing test files (cancel_abort, claude_strict_mcp, sse_two_chunk) — MEDIUM.
6. Add model existence pre-check before ollama run to prevent accidental pulls — MEDIUM.
7. Run target-env E2E on Linux + 9router/AionUI when infra available — PENDING.
8. Quota reset needed for opencode live eval (not blocking; zero-quota constraint respected).