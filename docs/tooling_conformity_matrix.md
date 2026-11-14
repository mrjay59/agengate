# Tooling Conformity Matrix — AgentGate Bridge

**Project:** `C:\Users\EDN\Documents\AION\project\agentgate`
**Spec:** `docs/spec_tooling.md` v0.2 (R1–R12, §0–§15)
**Date:** 2026-11-09
**Verification:** `cargo test --lib` 34/34 passed (unit). E2E against 9router/AionUI **pending target env access**.

---

## 1. CLI Provider Capability Mapping

| Capability | OpenCode | Claude | Gemini | Ollama (native) |
|---|---|---|---|---|
| **MCP Bridge (stdio)** | ✅ Config injection `OPENCODE_CONFIG_CONTENT` | ✅ `--mcp-config <file> --strict-mcp-config` (P1a) | ✅ `GEMINI_CLI_SYSTEM_SETTINGS_PATH` file path (P1a) | N/A (native proxy) |
| **Disable Builtins** | ✅ `excludeTools: ["*"]` per-run config | ✅ `--strict-mcp-config` isolates to agentgate MCP | ✅ Settings file `excludeTools: ["*"]` | N/A |
| **Reasoning Effort** | `reasoning_effort: "low\|medium\|high"` | Not native (uses `thinking` budget) | Not native (uses `thinking` budget) | `reasoning_effort` via native API |
| **Tool Calls (SSE)** | ✅ Emits `tool_calls` NDJSON | Via MCP bridge | Via MCP bridge | ✅ Native `tool_calls` stream |
| **Streaming Reasoning** | ✅ `thinking` blocks in NDJSON | Via MCP bridge | Via MCP bridge | ✅ Native `reasoning` stream |
| **Session Persistence** | ✅ Parked process + resume | ✅ Parked process + resume | ✅ Parked process + resume | N/A (stateless) |
| **Windows `.cmd` Wrapper** | ✅ `opencode.cmd` via `cmd /C` | ✅ `claude.cmd` via `cmd /C` | ✅ `gemini.cmd` via `cmd /C` | N/A |
| **Windows Process Tree Kill** | ✅ `taskkill /F /T /PID` | ✅ `taskkill /F /T /PID` | ✅ `taskkill /F /T /PID` | N/A |
| **Temp Dir Isolation** | ✅ `temp_dir()/agentgate/sessions/{id}/` | ✅ `temp_dir()/agentgate/sessions/{id}/` | ✅ `temp_dir()/agentgate/sessions/{id}/` | N/A |

---

## 2. Spec § Compliance Status

| Spec § | Requirement | OpenCode | Claude | Gemini | Ollama |
|---|---|---|---|---|---|
| §7.4 | SSE `tool_calls` 2-chunk format | ✅ (P0-2 done) | ✅ via bridge | ✅ via bridge | ✅ native |
| §9.6 | `notifications/cancelled` abort | ✅ (P0-3 done) | ✅ via bridge | ✅ via bridge | N/A |
| §9.7 | Keepalive during hold | ✅ (P0-4 done) | ✅ via bridge | ✅ via bridge | N/A |
| §10.4 | Resume parked run | ⚠️ Stage 1 done, Stage 2 in progress | Same | Same | N/A |
| §11.3 | Empty cwd per session | ✅ | ✅ | ✅ | N/A |
| §12.2 | OpenCode disable + MCP | ✅ config injection | N/A | N/A | N/A |
| §12.3 | Gemini settings file per-run | N/A | N/A | ✅ (P1a) | N/A |
| §12.4 | Claude `--mcp-config --strict-mcp-config` | N/A | ✅ (P1a) | N/A | N/A |
| §12.5 | Ollama native proxy | N/A | N/A | N/A | ✅ |
| §13 | Guard fail-closed 503 | ❌ P1-2/P1-6 pending | ❌ P1-2/P1-6 pending | ❌ P1-2/P1-6 pending | ⚠️ probe capabilities |

---

## 3. Reasoning_effort Mapping (RS5)

| Provider | Native Param | AgentGate Internal | Notes |
|---|---|---|---|
| OpenCode | `reasoning_effort` | `reasoning_effort` (passthrough) | `low` \| `medium` \| `high` |
| Claude | `thinking` (budget tokens) | Map `reasoning_effort` → `thinking.budget_tokens` | `low`=1024, `medium`=4096, `high`=16384 |
| Gemini | `thinking` (budget tokens) | Map `reasoning_effort` → `thinking.budget_tokens` | Same as Claude |
| Ollama (native) | `reasoning_effort` | `reasoning_effort` (passthrough) | Via native `/api/chat` |

---

## 4. UX Grouping (Reasoning + Tool View Steps)

| Aspect | Before (fragmented) | After (grouped) | Status |
|---|---|---|---|
| `thought` / `view steps 1` per tool | ✅ One per tool_call batch | ❌ Single block | ✅ Fixed in parser+coalesce |
| `thought complete` + `view steps N` grouped | ❌ | ✅ | ✅ Fixed in parser+coalesce |
| SSE `tool_calls` ordering (P0-2) | Preserved | Preserved | ✅ Verified |
| RS0 ordering (reasoning before text) | Preserved | Preserved | ✅ Verified |

---

## 5. E2E Verification Status

| Test Scenario | Status | Blocker |
|---|---|---|
| 9router/AionUI: grouped `thought complete`/`view steps N` | ⏳ Pending | No target env access |
| 9router/AionUI: SSE `tool_calls` 2-chunk parsing | ⏳ Pending | No target env access |
| Windows process-tree kill + cleanup | ⏳ Pending | Windows CI/env not available |
| P0-1b ID mapping (mcp_id ↔ tool_call_id) resume | ⚠️ Stage 1 done | P0-1b Stage 2 in progress |
| `guard.enforce` fail-closed 503 | ❌ Not implemented | P1-2/P1-6 pending |

---

## 6. Known Gaps / Action Items

| Gap | Priority | Owner | Target |
|---|---|---|---|
| P1-2/P1-6: Fail-closed guard (`503 tool_isolation_unavailable`) | P1 | Backend Dev | Post P0-1b |
| P1-3: Linux `/tmp` fallback + `.cmd` handling consistency | P1 | Backend Dev | Post P0-1b |
| P0-1b Stage 2: Parked CLI persistence (replay fallback) | P0 | Backend Dev | In progress |
| E2E 9router/AionUI validation | P0 | QA / Lead | Requires deploy |
| Windows CI runner for process-tree kill test | P1 | Infra | Separate task |

---

## 7. References

- `docs/spec_tooling.md` — Spec v0.2
- `docs/spec_reasoning.md` — RS0–RS5
- `docs/audit_spec_tooling.md` — Audit findings + 2026-11-09 updates
- `docs/spec_tooling_progress.md` — Implementation progress log
- `compat/clients.json` — CLI version catalog
- `compat/claude-1.0.0.json`, `compat/opencode-1.0.0.json`, `compat/gemini-1.0.0.json`, `compat/ollama-1.0.0.json` — Capability mapping templates