# Audit §11 Evidence Templates — 2026-11-09

**Purpose:** Standard templates for E2E evidence per `audit.md §11` Testing & Regression Checklist.
**Usage:** Copy template per scenario, fill all fields, attach artifacts (logs, screenshots, hashes). Only close checklist items with completed evidence.

---

## Template A — SSE tool_calls 2-chunk (§7.4)

| Field | Value |
|---|---|
| Scenario | SSE `tool_calls` 2-chunk parsing |
| Spec ref | §7.4 |
| Target env | 9router + AionUI |
| Expected chunk1 | `id` + `name` + `arguments:""` |
| Expected chunk2+ | `arguments` fragments ≤512B, `name:null` |
| Expected final | `finish_reason="tool_calls"` |
| Actual raw SSE | [attach `sse_raw_<scenario>.txt`] |
| PASS/FAIL | ☐ |
| Evidence loc | [path/to/artifact] |
| Date / tester | |
| Notes | |

---

## Template B — notifications/cancelled → abort + kill + cleanup

| Field | Value |
|---|---|
| Scenario | `notifications/cancelled` during held MCP `tools/call` |
| Spec ref | §9.6-9.7 |
| Target env | Linux / Windows |
| Expected: pending call | Woken with error (`ToolResult::error("cancelled")`) |
| Expected: process tree kill | `taskkill /F /T /PID <pid>` (Windows) / child kill (Linux) |
| Expected: cleanup dir | `temp_dir()/agentgate/sessions/{id}/work` removed |
| Expected: run stopped | `RunState::Done`/`Error`, HTTP 200 `isError:true` |
| Actual process tree log | [attach `process_tree_<scenario>.log`] |
| Actual temp dir state | `ls temp/agentgate/sessions/{id}/` → [empty / not-empty] |
| Actual SSE error | [attach `cancel_sse_<scenario>.txt`] |
| PASS/FAIL | ☐ |
| Evidence loc | [path/to/artifact] |
| Date / tester | |
| Notes | |

---

## Template C — Keepalive during MCP hold (>45s)

| Field | Value |
|---|---|
| Scenario | MCP `tools/call` hold > 45s, keepalive on transport |
| Spec ref | §9.7 |
| Target env | 9router / proxy |
| Expected keepalive | `: keepalive` every ≤15s |
| Expected: no proxy timeout | Connection held >45s |
| Actual transport log | [attach `keepalive_<scenario>.log`] |
| Actual hold duration | ____ seconds |
| PASS/FAIL | ☐ |
| Evidence loc | [path/to/artifact] |
| Date / tester | |
| Notes | |

---

## Template D — Batch 2 parallel tool calls

| Field | Value |
|---|---|
| Scenario | Batch 2 tool calls parallel → both resolve (not truncate) |
| Spec ref | P0/P2 batching |
| Target env | 9router + AionUI |
| Expected: call 1 | Resolved, result present |
| Expected: call 2 | Resolved, result present |
| Expected: no truncate | `tool_calls` not dropped to 1 |
| Actual SSE log | [attach `batch_<scenario>.txt`] |
| Actual tool_call_id gen | Generator used? ☐ CLI raw ☐ |
| PASS/FAIL | ☐ |
| Evidence loc | [path/to/artifact] |
| Date / tester | |
| Notes | |

---

## Template E — Live resume vs replay fallback

| Field | Value |
|---|---|
| Scenario | Resume parked run without respawn |
| Spec ref | §10.4-10.6 |
| Target env | Opencode + MCP |
| Expected: no new spawn | `log: "parked resumed"` |
| Expected: MCP hold resolved | Continuation awaited |
| Expected: replay fallback | Fresh session + `cli_gen++` on replay |
| Actual log | [attach `resume_<scenario>.log`] |
| PASS/FAIL | ☐ |
| Evidence loc | [path/to/artifact] |
| Date / tester | |
| Notes | |

---

## Template F — Windows process-tree kill

| Field | Value |
|---|---|
| Scenario | `taskkill /F /T /PID <pid>` on `opencode.cmd`/`claude.cmd`/`gemini.cmd` |
| Spec ref | P1-3 |
| Target env | Windows CI |
| Expected: process tree killed | No orphan child processes |
| Expected: fallback `child.kill()` | If `taskkill` unavailable |
| Actual `taskkill` output | [attach `taskkill_<scenario>.log`] |
| PASS/FAIL | ☐ |
| Evidence loc | [path/to/artifact] |
| Date / tester | |
| Notes | |

---

## Template G — Linux `/tmp` fallback + `.cmd` handling

| Field | Value |
|---|---|
| Scenario | `temp_dir()` consistent; `.cmd` handling on Windows only |
| Spec ref | P1-3 |
| Target env | Linux / Windows |
| Expected: no `.cmd` on Linux | `temp_dir()` used |
| Expected: `.cmd` on Windows | PATHEXT consistency |
| Actual env dump | [attach `env_<scenario>.log`] |
| PASS/FAIL | ☐ |
| Evidence loc | [path/to/artifact] |
| Date / tester | |
| Notes | |

---

## Template H — SHA256 artifact checksum

| Field | Value |
|---|---|
| Artifact path | `target/release/agentgate.exe` |
| Size (bytes) | ______ |
| SHA256 | ______ |
| Build date | ______ |
| CI workflow run ID | ______ |
| Target triple | ______ |
| Verified by | ______ |
| Date | ______ |

> **CI STATUS (2026-11-09):** GitHub Actions run #1 (commit 6a70ac5) **FAILED**. Artifacts/checksums NOT available for verification. Do not close this checklist item until CI passes + artifacts+checksums verified per `scripts/pre_release_checklist.md §7` and `audit.md §11`. Track in `docs/AUDIT_TRIAGE_ADDENDUM.md` CI Run Status table.

---

## Template I — Checklist Closure Sign-off

| Checklist item (§11) | Evidence ref | PASS/FAIL | Date | Tester |
|---|---|---|---|---|
| opencode direct: no spawn without config | | ☐ | | |

> **Eval CLI Note (2026-11-09, Lead):** `cargo run` server active → stopped. Opencode free usage limit (beam-free) hit → eval output blocked. Source fix applied + binary rebuilt (2026-11-09), still blocked by free usage limit. Cannot complete E2E opencode direct test (§11 checklist item 1) until quota reset or paid tier.
| SSE tool_calls parsing client | Template A | ☐ | | |
| tools/call hold >45s keepalive | Template C | ☐ | | |
| notifications/cancelled + cleanup | Template B | ☐ | | |
| Batch 2 parallel tool calls | Template D | ☐ | | |
| Resume parked run no respawn | Template E | ☐ | | |
| Windows no Access denied cwd | | ☐ | | |
| guard.enforce fail-closed 503 | | ☐ | | |
| Linux /tmp fallback valid | Template G | ☐ | | |
| Windows process-tree kill | Template F | ☐ | | |
| SHA256 artifact checksum | Template H | ☐ | | |
