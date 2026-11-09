# Stage 2: Parked CLI Process/Session Persistence — Design + PR-sized Steps

**Spec:** `docs/spec_tooling.md` §10.4–§10.6 (Resume, Fallback Replay, TTL/cleanup).  
**Audit:** `audit.md` P0-1b Stage 2 open.  
**Status (2026-11-09, updated):** P0-1/1b Stage 1, P0-2/3/4, P1/P2 pragmatis done; replay headers + `replay_fallback` logging added. **PR-S4 code-wired (opencode first, capability-gated, replay fallback mandatory):** continuation registry (`ContinuationOutput`/`register|take|remove|rekey_continuation`), opencode park-early + `wait_for_continuation`, `CliProviderImpl.send_chat_completion` registers continuation and keeps MCP/provider session alive on `tool_calls`+capable, `service::resume_run` resolves real MCP holds then awaits continuation (`run_id`/`session_key`, timeout) → response or `replay_fallback{continuation_*}`. **S5 partial (reported complete by BD, 137 tests claimed; lead verified core suites + added increments):** continuation TTL sweep, replay session rotation (clear `session_id`/`agentgate_run_id`), cleanup on continuation/provider terminal paths, `cancel_run`/evicted-key cleanup, orphan session *dir* sweep at startup; **remaining:** PID/process orphan sweep, full `cli_gen++`/fresh CLI session guarantees per adapter, end-to-end TTL/cleanup/process-tree verify incl multi-step continuation re-park/state, targeted leak tests. **E2E pending** (MCP-hold→CLI lanjut, Linux/9router/AionUI/VSC Copilot, hold>45s/cancel/batch); streaming continuation partial. Tests green (check, lib 55, bridge_mcp_hold/fail_closed/bridge_resolve/sse/spec_scenarios, stage2_continuation_test 2).

## 1. Goal (spec-aligned)

- When a tool-capable turn yields `tool_calls`, the run **parks** and the **CLI process/session that produced the call stays alive** (no default respawn). MCP `tools/call` may hold (`oneshot`) while waiting for client results.
- On **Resume** (trailing `role:"tool"` messages, all `tool_call_id`s resolve to the same waiting run/batch): deliver results to the *live* parked session/process (`BridgeSession::send_tool_result` / continuation path as applicable), transition `Resuming → Running`, and **stream next events** from that same session. Do **not** re-invoke provider with flattened history when the parked session is alive.
- On **Replay fallback** (§10.5): if IDs not found, restart/expiry, parked process/session lost/unhealthy, or continuation unsupported — **rotate session** (`cli_gen++` / new CLI session, provider-dependent), replay flattened history, log `warn replay_fallback{reason}`, set `X-Agentgate-Replay: 1` + `X-Agentgate-Replay-Reason` (already partial in API/service).
- **Cleanup** (§10.6, all terminal paths): cancel/kill process tree → wait → remove `by_run`/`by_call`/parked entries → remove run workdir → release semaphores → close provider/MCP sessions. Enforce `park_ttl_secs`, `call_ttl_secs`, `max_parked_runs`, `max_run_lifetime_secs`, `max_steps`.

## 2. Current architecture (observed)

- **`BridgeServer` (`src/bridge.rs`)**: MCP sessions, `pending_calls` (oneshot for HTTP-held `tools/call`), `parked_runs`/runs, `provider_sessions` (`Arc<dyn BridgeSession>`), cleanup task. `hold_tool_call` waits with timeout; `resolve_*` signals oneshot. `Run` has state/messages/tools/TTL but limited session linkage.
- **`RunRegistry` (`src/run.rs`)**: agentic run model, `resume_run` appends `ToolResult` to run messages; separate from live provider continuation.
- **`ServiceRegistry` (`src/service.rs`)**: classifies Native/Resume/Replay/NewBridge/NewChatOnly. `resume_run` resolves tool results + releases MCP hold but then **`call_provider_native(provider_id, request)`** (replay-like) instead of continuing live session. `new_bridge_run` parks `bridge::Run` when tool_calls present.
- **`CliProviderImpl` (`src/provider.rs`)**: bridge-mode turn does send → `wait_for_completion` → close session (unless `keep_alive`); registers provider session with `BridgeServer`. No “parked continuation” API exposed to service.
- **`BridgeSession` trait (`src/provider/bridge.rs`)**: `send_user_message`, `wait_for_completion`, `send_tool_result`, `close`, isolation checks (per impl). Adapters (opencode/claude/gemini/ollama_cli) vary.

**Gap:** service/bridge do not keep a *continuable* session+stream handle across the HTTP response boundary; `send_chat_completion` is request/response, not resumable.

## 3. Design principles

- **Safety over completeness:** preserve replay fallback; never hang; never leak processes/workdirs.
- **Small PRs:** each compiles, tests green (`cargo test --lib` + targeted integration), no behavior regression on existing paths.
- **Feature-gated/compatible rollout:** prefer explicit capability checks (`session supports continuation`, `keep_alive`/live handle present) with fallback to current replay behavior.
- **Observability:** structured events for `run.parked`, `run.resume_live`, `run.resume_replay_fallback`, `run.cleanup`.

## 4. PR-sized steps

### PR-S1 — Session/run linkage metadata (foundation, low risk)
**Goal:** Make it possible to locate a live provider/MCP session for a parked run/call.  
**Changes (scoped):**
- Add optional linkage fields to `bridge::Run` and/or parked registry entries: `mcp_session_id`, `provider_session_key`, `tool_call_ids`/batch ids, timestamps, `continuation_capable` flag (no behavior change).
- Ensure `park_run` / MCP hold paths populate linkage when known; keep backward-compatible constructors/defaults.
- Unit tests: run park/resume bookkeeping, lookup by `tool_call_id`/`run_id`.
**Acceptance:** `cargo check`, `cargo test --lib`, `bridge_mcp_hold_test` green; no change to resume behavior.

### PR-S2 — Provider session liveness + replay-on-lost (spec §10.5)
**Goal:** Explicit “parked session lost ⇒ replay fallback” with reason codes.  
**Changes:**
- In `BridgeServer`/service resume path, check `provider_sessions` / session liveness before attempting live resume; if missing/closed/unhealthy, `replay_fallback{reason=parked_session_lost|process_lost|expired}` and proceed with flattened replay (current behavior).
- Ensure `X-Agentgate-Replay-Reason` can reflect specific reason when available (plumb optional reason through API response metadata without breaking JSON schema).
- Tests: unit/integration for “parked run exists but provider session gone → replay + header/reason”.
**Acceptance:** replay fallback deterministic; no live-resume attempt on dead session.

### PR-S3 — Continuation-capable provider/bridge session API (design seam)
**Goal:** Introduce a minimal interface for “keep session open and continue after tool results” without breaking existing `send_chat_completion`.  
**Changes:**
- Extend `BridgeSession` (or add companion trait, e.g. `ContinuableBridgeSession`) with capability probe + continuation method(s): e.g. `supports_continuation()`, `submit_tool_results(...)`/reuse `send_tool_result` per call, and a way to receive next `ProviderResponse`/events (adapter-specific; start with opencode/claude where feasible).
- `CliProviderImpl`: optional path that, when continuation supported and run is parking, **does not close** session after emitting tool_calls; returns handle/registration token to service.
- Default off/guarded by config/capability; fallback to current one-shot + replay.
**Acceptance:** existing behavior unchanged when continuation unavailable; new code paths covered by targeted tests/mocks.

### PR-S4 — Wire live resume in service/bridge (core §10.4)
**Goal:** Resume uses live parked session when available.  
**Changes:**
- `service::resume_run`: if parked run + live continuable session found, deliver tool results (`send_tool_result`/batch), await next completion/events, return response / continue streaming; **do not** `call_provider_native` on the happy path.
- Handle partial batch results per spec (`resume.partial_grace_ms`, default 0) where feasible; otherwise document.
- On continuation error/timeout/lost process mid-resume → cleanup + `replay_fallback{reason=resume_continuation_failed}`.
- Streaming: support continuing SSE stream context if client reconnect semantics allow (or document v1 limitation).
**Acceptance:** integration test (mock/bridge harness) shows resume without provider respawn when session alive; replay fallback when lost.

### PR-S5 — Session rotation on replay + cleanup hardening (§10.5, §10.6)
**Goal:** Spec-correct replay rotation and leak-free cleanup.  
**Changes:**
- Replay path: request new CLI session (`cli_gen++` semantics or fresh `session_id`/MCP session) so stale CLI context is not reused; provider adapters implement rotation hook where applicable.
- Harden cleanup on Done/Failed/Expired/Cancelled/Resume-fallback: kill process tree (Windows `taskkill /F /T` best-effort already present in places), wait, remove by_run/by_call/parked/provider session refs, delete run workdir, release semaphores/permits, stop MCP session. Sweep orphan run dirs/PIDs on startup.
- Tests: TTL expiry cleanup, cancel cleanup, replay creates new session id/generation marker.
**Acceptance:** no leaked sessions/workdirs in test harness; TTLs enforced consistently.

### PR-S6 — E2E validation + docs/audit closure
**Goal:** Verify against target environments and close audit gaps.  
**Changes:**
- E2E: Linux bridge, 9router/AionUI (SSE + UX grouping), VSC Copilot workdir/tooling, hold >45s keepalive, cancel, batch 2 tool calls, resume live vs replay fallback.
- Update `audit.md` §11, `docs/spec_tooling_progress.md`, conformity matrix; record CI all-platform artifact hashes.

## 5. Risks / dependencies
- **Provider heterogeneity:** opencode/claude/gemini/ollama_cli have different process/protocol models; continuation may land adapter-by-adapter.
- **Streaming resume:** mid-stream reconnect/resume buffer is out of scope unless explicitly added (spec notes v1 limitation for idempotent retry buffer).
- **Conflict surface:** `src/service.rs`, `provider.rs`, `bridge.rs`, `run.rs`, `api.rs`, provider adapters — coordinate PR order and ownership.

## 6. Recommended next action
Start **PR-S1** (metadata/linkage, no behavior change) + **PR-S2** (liveness check + deterministic replay reason) as the safest foundation, then design the exact continuation interface from real adapter capabilities (**PR-S3**). Do not wire live resume (**PR-S4**) until S3 contract is proven in at least one adapter path with tests.
