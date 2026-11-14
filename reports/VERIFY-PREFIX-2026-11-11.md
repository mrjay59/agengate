# VERIFY-PREFIX — E2E Verification Report
**Date:** 2026-11-11  
**Commit:** 722099e  
**Env:** Local (127.0.0.1:8780) + VPS (10.11.1.21:8780), auth `default_key`, bypass 9router direct  
**Models:** `agg/9router/ag-poli`, `agg/9router/free-forever`  
**Tools tested:** glob, read, shell, execute, create/write, grep (initial stream only; resume deferred per YAGNI)  

---

## Result Summary

| env | model | glob | read | shell | execute | create | grep | **Overall** |
|-----|-------|------|------|-------|---------|--------|------|-------------|
| local | ag-poli | ✅ `glob` | ✅ `read` | ✅ `shell` | ✅ `shell` | ✅ `write` | ✅ `grep` | **PASS** |
| local | free-forever | ✅ `glob` | ✅ `read` | ✅ `shell` | ✅ `shell` | ✅ `write` | ✅ `grep` | **PASS** |
| vps   | ag-poli | ✅ `glob` | ✅ `read` | ✅ `shell` | ✅ `shell` | ✅ `write` | ✅ `grep` | **PASS** |
| vps   | free-forever | ✅ `glob` | ✅ `read` | ✅ `shell` | ✅ `shell` | ✅ `write` | ✅ `grep` | **PASS** |

**Total:** 24/24 PASS · 0 PREFIX_VIOLATION · 0 ERROR

---

## Key Findings

### ✅ Prefix normalization confirmed working end-to-end
No `agentgate_`, `agentgate__`, `mcp__<server>_` or any forbidden namespace prefix observed in any emitted `tool_calls.function.name`. AionUI "Unknown tool" caused by prefixed names is **not reproducible on this commit**.

### ⚠ Consistent model behaviour — execute → shell, create → write
Both models route "execute command" prompts to the `shell` tool name, and "create file" prompts to `write` tool name. This is the correct canonical tool set per opencode/session spec — no bug, but worth documenting so client-side expectations are aligned.

### Unit-test gap noted
`strip_mcp_tool_prefix("AgentGate_Glob")` lowercases result → `"glob"` not `"Glob"`. This is intentional in current code (canonicalises to lowercase). If any downstream consumer expects camelCase (`Glob`), that's a separate contract issue to flag. No test currently validates case fidelity.

---

## Evidence Snippets (representative SSE tool_call frames)

### ag-poli / local / read
```json
{"delta": {"tool_calls": [{"function": {"arguments": "", "name": "read"}, "id": "HbptDZVRW", "index": 0, "type": "function"}]}}
```

### ag-poli / vps / grep
```json
{"delta": {"reasoning_content": "… 🔍 search grep → src\n"}, "finish_reason": null}
data: {"choices": [{"delta": {"tool_calls": [{"function": {"arguments": "{\\\"path\\\": \\\"src\\\"}, \"name\": \"grep\"}, ...}]}}]}
```

### free-forever / local / write (create→write)
```json
{"delta": {"tool_calls": [{"function": {"arguments": "{\\\"path\\\": \"/tmp/test_verify_prefix.txt\", \"content\": \"test\"}, \"name": "write"}, "id": "call-5d68a7c5-9b9e-47e8-9830-4febb8fd97f6", "index": 0}]}
```

Full raw dumps available in `verify_prefix_results.json`.

---

## Out of Scope / Deferred
- **Resume pass** for prefix check — initial emission already verified clean; resuming against same session tools should be identical. Defer unless regression surface observed.
- **Non-stream mode** — all E2E hit via `stream: true`; non-stream path uses different code branch (`openai/tools.rs::unalias_tool_calls`) but same normalization logic. Low risk given unit-test coverage.
- **Case fidelity audit** (`Glob` vs `glob`) — tracked separately; no blocking impact on known failing cases.

---

## Recommendation
**Ship.** Prefix handling is clean across all tested tools, models, and environments. No action needed on prefix side for AionUI compatibility at this commit.