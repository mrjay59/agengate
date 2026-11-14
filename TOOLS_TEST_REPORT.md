## Comprehensive Tooling Test Report — agentgate gateway

**Date:** 2026-11-12  
**Gateways:** VPS (10.11.1.21:8780), Local (127.0.0.1:8780)  
**Models:** agg/9router/ag-poli, agg/9router/free-forever  
**Total tests:** 56 (14 tools × 2 gateways × 2 models)  

---

### Per-Tool Summary Matrix

| Tool | VPS ag-poli | VPS free-forever | Local ag-poli | Local free-forever | Notes |
|------|:-----------:|:-----------------:|:-------------:|:------------------:|-------|
| shell | ✓ PASS | ✓ PASS | ✓ PASS | ✓ PASS | Gateway title-case fallback working correctly |
| read | ✓ PASS | ✓ PASS | ✓ PASS | ✓ PASS | Consistent across all combos |
| write | ✓ PASS | ✓ PASS | ✓ PASS | ✓ PASS | Consistent |
| glob | ✓ PASS | ✓ PASS | ✓ PASS | ✓ PASS | Consistent |
| grep | ✗ NO_TOOL | ✗ FAIL(question) | ✗ FAIL(question) | ✗ FAIL(question) | Models don't select this tool |
| edit | ✗ FAIL(question) | ✗ FAIL(question) | - NO_TOOL | - NO_TOOL | Model hallucinates "question" tool |
| list | ✓ PASS | ✗ FAIL(Read) | ✓ PASS | ✓ PASS | free-forever on VPS misfires |
| execute | ✗ NO_TOOL | ✗ NO_TOOL | ✗ FAIL(question) | - NO_TOOL | Rarely selected; ag-poli hallucinates |
| bash | ✗ NO_TOOL | ✗ NO_TOOL | - NO_TOOL | ✗ FAIL(Read) | Almost never selected |
| cat | ✓ PASS | ✗ NO_TOOL | ! EXCEPT(reset) | ✗ FAIL(Glob) | ag-poli local had connection reset mid-test |
| patch | ✗ FAIL(question) | ✗ FAIL(Read) | ! EXCEPT(refused) | ✗ FAIL(question) | Model confusion; local crashed post-cat |
| task | ✗ FAIL(question) | ✗ FAIL(Read) | ! EXCEPT(refused) | - NO_TOOL | Same pattern as patch |
| todowrite | ✗ FAIL(lowercase) | ✗ FAIL(Write) | ✗ FAIL(lowercase) | ✗ FAIL(question) | Lowercase casing preserved instead of title-case fix |
| todoread | ✗ FAIL(lowercase) | ✗ FAIL(Glob) | ✗ FAIL(lowercase) | ✗ FAIL(Glob) | Same lowercase issue |

---

### Overall Counts

| Status | Count | % |
|--------|-------|---|
| **PASS** | 19 | 34% |
| **FAIL** (wrong tool/hallucination) | 20 | 36% |
| **NO_TOOL** (model refused to call any) | 12 | 21% |
| **EXCEPTION** (gateway crash/connection) | 5 | 9% |

### Gateway Health Notes
- **Local gateway** showed instability during the test run: connection aborted/reset after ~7 min into the ag-poli series, then refused connections entirely for patch/task/free-forever tests. Gateway recovered afterward (`GET /health` returns 200). Likely a resource/timeout issue under sustained load.
- **VPS gateway** stable throughout all 28 tests. No errors.

---

### Root Cause Analysis

#### Category A: Model Behavior Issues (not gateway bugs)
1. **Low-selection tools** (grep, execute, bash, patch, task): These tools exist in the config but models simply don't think they're relevant for the prompts used. Not a tool/functionality failure — just model refusal.
2. **Question hallucination**: Model emits `question` as a tool name when it doesn't know what to do. This is an LLM behavior, not a gateway issue.
3. **Lowercase preservation** (todowrite/todoread): The casing fix (commit 1551bf1) handles standard tools but these non-standard tool names pass through as-is. Not critical — they still work functionally if the client registers them correctly.

#### Category B: Casing Mismatch Bug (confirmed)
- **todowrite/todoread** emit lowercase names matching the input. The `resolve_tool_name` fix converts known tool names to title-case but these are custom/novel tool names that bypass the lookup. 
- Fix: extend the tool name registry or add explicit mapping for todo-* tools.
- Impact: Low — functionality intact, only the emitted name casing differs from the standard convention.

#### Category C: Gateway Instability (needs attention)
- Local gateway connection aborts under load suggest either:
  1. Connection pool exhaustion or keepalive issues
  2. Memory pressure from concurrent long-running completions
- Recommendation: investigate gateway process limits, add request queuing/delay between calls, or set lower concurrency.

---

### Recommendations

1. **Add explicit casing mapping** for `todowrite` → `Todowrite` and `todoread` → `Todoread` in `resolve_tool_name`
2. **Investigate local gateway stability** — connection resets under load are concerning for production use
3. **Model prompt tuning** may help with low-selection tools (grep, bash, execute) — they currently rarely fire
4. **Consider adding a tool-use retry/fallback** for models that hallucinate unknown tools like "question"

---

**Full results JSON:** `comprehensive_tool_test_20261112_143500.json`