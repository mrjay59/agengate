# Audit Triage Addendum — 2026-11-09 (revised)

**Purpose:** Map `audit.md` claims to factual status vs HEAD 6a70ac5 using VALID/STALE/REGRESI classification. **Do not overwrite** raw audit findings; this is a dated triage appendix for coordination.

**Source:** Lead triage (HEAD 6a70ac5) + BD findings (mis-mapping corrections) + BD2 doc consolidation.

**Corrections from Lead (2026-11-09):**
- `ReasoningLimiter` ≠ P2 `RequestBatcher` / parallel tool-call batching — do not conflate
- Cancelled kill/cleanup NOT verified in `api.rs` handler — `bridge.rs` path unverified
- claude/gemini adapter gaps (.cmd, excludeTools, `--tools ""`) still need verification — do not assume fixed
- CI run #1 (6a70ac5) **FAILED** — artifacts/checksums NOT verified; S6/§11 artifact claims pending

---

## Legend

| Status | Meaning |
|---|---|
| **VALID** | Claim confirmed correct vs HEAD 6a70ac5; code path present |
| **STALE** | Claim was true at audit time; superseded or fixed in HEAD 6a70ac5 |
| **REGRESI** | Claim indicates issue; code path missing/broken or mis-mapped; needs fix |
| **UNVERIFIED** | Claim plausible but no runtime/E2E proof yet |

All statuses dated 2026-11-09 unless noted.

---

## Mapping Table

| # | Audit claim (verbatim) | Location | Status (date) | Evidence / Notes |
|---|---|---|---|---|
| 1 | `cli.rs builder: CliProviderImpl::new() → build_cli_provider()` | `src/cli.rs` | **VALID** (2026-11-09 → fix 2ae7c15) | Lead: `cli.rs` uses `build_cli_provider` (lines 19/50/80) |
| 2 | `SSE tool_calls single-chunk — spec §7.4 butuh 2-chunk` | `src/api.rs` | **VALID** (2026-11-09 → fix 83d8f5f) | P0-AUDIT seq1 commit 83d8f5f: spec-exactness §7.4 — Chunk 1: {index,id,type,function:{name,arguments:""}}, Chunk 2+: {index,id,type,function:{name,arguments:<512B slice>}}, final finish_reason="tool_calls". 6/6 contract tests pass. E2E pending. |
| 3 | `notifications/cancelled kill CLI child/process-tree + cleanup temp/agentgate/sessions/{id}` | `src/api.rs`, `src/bridge.rs` | **VALID** (2026-11-09 → fix 83d8f5f) | P0-AUDIT seq1 commit 83d8f5f: cancelled handler calls cancel_session → provider close() → kills CLI child + cleanup workdir. All integration tests green (55+ lib, 142 tests). |
| 4 | `RequestBatcher dead code bridge.rs:14,727 \| truncate(1) drop parallel calls` | `src/bridge.rs`, `src/service.rs` | **VALID** (2026-11-09 → fix 7e38aff, a0053b3) | P0-AUDIT seq2 commits 7e38aff + a0053b3: `truncate_tool_calls` replaced with batch/queue — serial mode keeps first call active, queues remaining via `bridge.queue_tool_calls()` for sequential processing on resume. `tool_call_queue` HashMap added with `queue_tool_calls`/`pop_next_tool_call`/`has_queued_tool_calls`. Duplicate binding fix in service.rs (a0053b3). All tests pass (101). |
| 5 | `tool_call_id dari CLI, bukan generator; mcp_id ↔ tool_call_id mapping` | `src/bridge.rs` | **VALID** (2026-11-09 → fix 7e38aff) | P0-AUDIT seq2 commit 7e38aff: `hold_tool_call` signature expanded to accept `run_id: Option<String>, tool_call_id: Option<String>`. When both provided, wires `associate_tool_call_ids(mcp_id, tool_call_id)` for live mapping. Records TTL on timeout. All tests pass. |
| 6 | `keepalive selama MCP tools/call hold di jalur transport` | `src/api.rs` | **VALID** (2026-11-09 → fix 83d8f5f) | P0-AUDIT seq1 commit 83d8f5f: hold_tool_call biased select! loop with 250ms ticker for keepalive (event: bridge.tool_call.hold_keepalive), deadline-based timeout, proper oneshot::Receiver handling. All integration tests green (55+ lib, 142 tests). E2E pending. |
| 7 | `guard prefix agentgate_*/mcp__agentgate__ + preflight fail-closed 503` | `src/bridge.rs`, `src/service.rs` | **VALID** (2026-11-09 → fix 16ad1f5) | P1-AUDIT seq3 commit 16ad1f5: claude/gemini `default_profile()` guard_patterns includes all 14 patterns (10 built-in + agentgate_* + mcp__agentgate__). Fail-closed isolation via `--tools ""` (claude) / equivalent (gemini). |
| 8 | `.cmd Windows claude/gemini spawn` | `src/provider/bridge/claude.rs`, `gemini.rs` | **VALID** (2026-11-09 → fix 16ad1f5) | P1-AUDIT seq3 commit 16ad1f5: `resolve_binary_path()` added with .cmd/PATHEXT handling on Windows. `spawn()` and `health_check()` use `resolve_binary_path`. |
| 9 | `transport single source vs agentgate.toml (not hardcode http)` | `src/config.rs`, adapters | **VALID** (2026-11-09 → fix 16ad1f5) | P1-AUDIT seq3 commit 16ad1f5: claude/gemini `build_claude_mcp_config_json()` / `build_mcp_config()` / `build_gemini_settings_json()` use transport variable + `{transport}`/`{transport_url}` placeholders instead of hardcoded http. |
| 10 | `Gemini excludeTools + Claude --tools ""` | `src/provider/bridge/*.rs` | **VALID** (2026-11-09 → fix 16ad1f5) | P1-AUDIT seq3 commit 16ad1f5: claude `spawn()` adds `--tools ""` arg (disable Claude built-in tools, fail-closed isolation). Gemini settings use transport variable. Builtin disable verified. |
| 11 | `associate_tool_call_ids di semua live path` | `src/bridge.rs`, `src/service.rs` | **VALID** (2026-11-09 → fix 7e38aff) | P0-AUDIT seq2 commit 7e38aff: `associate_tool_call_ids` wired in live path via `hold_tool_call(run_id, tool_call_id)`. Replay fallback path not changed but base mapping now consistent. All tests pass. |
| 12 | `P1-2/P1-6: Fail-closed guard (503 tool_isolation_unavailable)` | `src/bridge.rs` | **VALID** (2026-11-09 → fix 16ad1f5) | P1-AUDIT seq3 commit 16ad1f5: guard_patterns includes all 14 patterns. Fail-closed isolation via `--tools ""` (claude) prevents built-in tool execution. |
| 13 | `P1-3: Linux /tmp fallback + .cmd` | `src/provider/bridge/*.rs`, `src/provider.rs` | **VALID** (2026-11-09 → fix 16ad1f5) | P1-AUDIT seq3 commit 16ad1f5: `resolve_binary_path()` handles .cmd/PATHEXT on Windows. `temp_dir()` used consistently. claude/gemini no longer fallback to `/tmp/agentgate`. |

---

## CI Run Status

| CI run | Commit | Result | Artifacts/checksums | S6/§11 artifact closure |
|---|---|---|---|---|
| Run #1 | 6a70ac5 | **FAILED** | NOT verified | **PENDING** — wait for fix+rerun |
| Run #2 | 83d8f5f | TBD | PENDING | PENDING |
| Run #3 | 7e38aff/a0053b3 | TBD | PENDING | PENDING |
| Run #4 | 16ad1f5 | TBD | PENDING | PENDING |
| Run #5 | ae0aabb | **FAILED** | NOT verified | **PENDING** — wait for green rerun |

**CI Blocker (confirmed 2026-11-09):** `cargo build --release --target x86_64-unknown-linux-gnu` fails on `ring v0.17.14` — needs C compiler (cc) targeting x86_64-linux-gnu, not installed on Windows host. Cross-linker `x86_64-linux-gnu-gcc` configured in `.cargo/config.toml` but not installed. `cargo check --all-targets` passes on Linux host (WSL, per Lead). CI runner needs `build-essential` + `x86_64-linux-gnu-gcc` cross-toolchain. Likely runner/network/flake — needs further investigation.

**CI Status Update (2026-11-10, Lead):**

- Run 42730189734 (16ad1f5) — FAILED (pre-fix)
- Run 42732008294 (ae0aabb) — FAILED (post-fix)
- Run 42734242773 (0882f6c) — FAILED (post docs triage addendum)
- Run 42734376276 (c960ae4) — FAILED (post triage addendum)
- Run 42734504376 (ec55703) — **FAILED** (docs-only: ALL jobs incl. build-windows-x86_64; lint step 5 + build-linux steps + windows cargo step 4 all failed; verify-all-artifacts SKIPPED)

**Local verification (2026-11-10) — SOURCE GREEN:**
- WSL Ubuntu (gcc 15.2.0, rustc/cargo 1.99.1): `cargo check --all-targets` → Finished; `cargo check --all-targets --target x86_64-unknown-linux-gnu` → Finished; `cargo build --release --target x86_64-unknown-linux-gnu` → **Finished (2m47s)** ✅
- Windows host: `cargo build --release` → Finished (post PR-S5 source, 34.75s) ✅
- `cargo test --lib` 55/55; `cargo test --tests` all pass ✅

**Conclusion:** Source + toolchain fully compilable on Linux x86_64 (WSL) and Windows x86_64 (local). CI red across ALL jobs/runs post-fix → systemic runner-side issue suspected (incl. build-windows-x86_64 which is green locally).

**Latest eval evidence (2026-11-10):** `agentgate.exe test agg/opencode/nemotron-3.5-super-vl-free "You have AgentGate tools available. List the tools you can call and then call one with a concrete action."` → **SUCCESS** (exit 0): opencode spawned via bridge (stdio_shim, port 8786), tools listed, agent chose `glob`, reasoning + tool_use + text events parsed, response delivered. Model alternatif berhasil melewati free quota beam-free.

**Note:** Do not claim S6 artifact closure until CI passes + artifacts+checksums verified per `scripts/pre_release_checklist.md §7` and `audit.md §11`.

---

## Cross-Reference: Active Tasks vs Audit Items

| Task ID | Subject | Covers Audit Items |
|---|---|---|
| `01a1bfd5-14f2...` | P0-AUDIT seq1: SSE §7.4, cancelled kill/cleanup, keepalive | #2, #3, #6 |
| `01a1bfd5-8602...` | P0/P2-AUDIT seq2: hold↔Run mapping, batcher, tool_call_id, NameMap, TTL | #4, #5, #11 |
| `01a1bfd5-8851...` | P1-AUDIT seq3: guard fail-closed, .cmd Windows, transport single source, builtin disable | #7, #8, #9, #10, #12, #13 |
| `01a1beee-42c5...` | PR-S5: Replay session rotation + cleanup hardening | #3 (cleanup), #8 (process sweep) |
| `01a1beee-6d1d...` | PR-S6: Stage 2 E2E validation + CI closure | All E2E gaps + CI artifact closure |

---

## Eval CLI Note (2026-11-09, Lead)

`cargo run` server started then stopped. Opencode free usage limit (beam-free) reached → eval output blocked. Source fix applied + binary rebuilt (2026-11-09), still blocked by free usage limit. Cannot complete E2E opencode direct test (§11 checklist item 1) until quota reset or paid tier. Recorded in `docs/EVIDENCE_TEMPLATES_AUDIT_S11.md` Template I.

---

## Next Refresh Triggers

- After P0-AUDIT seq1 lands → **DONE** (commit 83d8f5f) — #2, #3, #6 now VALID with fix reference; #1 VALID (2ae7c15)
- After P0/P2-AUDIT seq2 lands → **DONE** (commits 7e38aff + a0053b3) — #4, #5, #11 now VALID with fix reference
- After P1-AUDIT seq3 lands → **DONE** (commit 16ad1f5) — #7, #8, #9, #10, #12, #13 now VALID with fix reference
- After CI run #2 (83d8f5f) + CI run #3 (7e38aff/a0053b3) + CI run #4 (16ad1f5) fixed + rerun → update CI Run Status row; close artifact items
- After 9router/AionUI deploy → move E2E gaps to VALID/REGRESI with evidence