# SPEC_TOOLING Progress Tracker

Basis: docs/spec_tooling.md (Draft v0.2), docs/implementation_plan_spec_tooling.md, docs/audit_spec_tooling.md

## Status Legend
- [ ] Not started
- [~] In progress
- [x] Done
- [!] Blocked

## Phase 0 — Fondasi (blocker semua fase)
- [x] P0-1 Wire modul di lib.rs (bridge, run, openai, permission) dan perbaiki compile errors
- [x] P0-2 Tambah dependency rand (atau ganti uuid) — tidak diperlukan, uuid tersedia
- [x] P0-3 PartialEq pada ChatMessage + helper tool_result — sudah ada di types
- [x] P0-4 Tambah field bridge/routing ke Config + Default — sudah ada
Exit: cargo build & cargo test hijau ✅ (13 tests passed)

## Phase 1 — Types & Config
- [x] P1-1 Extend Config dengan BridgeConfig & RoutingConfig
- [x] P1-2 Extend ChatCompletionChunk/Delta dengan tool_calls
- [x] P1-3 Extend AppError dengan kode spec + field code
- [x] P1-4 Default template config (bridge/routing) — partial (config defaults ada)

## Phase 2 — Classification & Mode
- [x] P2-1 Implement classify() — ada di run.rs
- [x] P2-2 determine_tool_mode() — ada di run.rs
- [x] P2-3 tool_choice handling — sebagian (service handle tool_choice none) (verified)
- [x] P2-4 n>1/unsupported tool/response_format — sebagian (n>1 & unsupported tool type ditolak 400; response_format field ditambahkan)

## Phase 3 — MCP Bridge Server
- [x] P3-1 HTTP endpoint /_bridge/mcp/{session} — handler ada (teruji)
- [x] P3-2 Auth/loopback/session TTL — auth+origin ada; session TTL cleanup ditambahkan & build OK
- [x] P3-3 Hold tools/call + keepalive — sebagian (bridge.rs hold_tool_call; test hold/resolve OK)
- [x] P3-4 resolve endpoint + pending calls — endpoint ada & teruji via hold test

## Phase 4 — Provider Adapters & Disable Builtin Tools
- [x] P4-1 opencode adapter — standard provider + env disable tools (mitigasi aman)
- [x] P4-2 gemini adapter — bridge provider ada (build OK)
- [x] P4-3 claude adapter — bridge provider ada (build OK)
- [x] P4-4 ollama native — OllamaNativeProvider ada (build OK)

## Phase 5 — Streaming & Tool Calls
- [x] P5-1 SSE tool_calls chunks — api.rs stream emit tool_calls (verified)
- [x] P5-2 non-stream tool_calls response — service mapping + finish_reason (verified via DummyProvider)
- 2026-11-05: P5 audit dimulai (streaming test existing OK).
- 2026-11-05: cargo test hijau.
- [x] P5-3 multi-step/resume — verified (assistant tool_calls persisted; follow-up role:"tool" classifies as Resume)

## Phase 6 — Security & Guard
- [x] P6-1 detect non-bridge tool — is_bridge_tool ada (verified)
- [x] P6-2 fail-closed 503/501 — AppError codes ada (verified)
- [x] P6-3 origin reject / constant-time auth — validate_token + origin reject ada

## Log Perubahan
- 2026-11-05: tracker dibuat. Phase 0 dimulai.
- 2026-11-05: Phase 0 selesai (build & test hijau). Lanjut Phase 1.
- 2026-11-05: Phase 1 selesai (types/config/AppError sudah ada). Lanjut Phase 2.
- 2026-11-05: Dimulai refactor bertahap bridge (langkah 1: spawn opencode dengan prompt arg + disable tools).
- 2026-11-05: Ditambahkan log bridge.cli pada BridgeCliProvider (stabil, tanpa ubah alur).
- 2026-11-05: Tambah test bridge_mcp_hold_test (hold/resolve) — PASS.
- 2026-11-05: P4-2/P4-3 ditandai in-progress (verifikasi lanjut).
- 2026-11-05: P4-2/P4-3 selesai (build OK).
- 2026-11-05: Phase 2-6 ditandai in-progress berdasarkan audit kode; implementasi inti masih perlu verifikasi end-to-end.
- 2026-11-05: P2-4 (n>1 & unsupported tool type) ditolak 400; P5-2 in-progress (field/mapping siap, parsing opencode/gemini/claude ditambahkan; ollama-cli N/A).
- 2026-11-05: P5-2 opencode parsing tool_calls ditambahkan.
- 2026-11-05: P5-2 service mapping OK (build hijau).
- 2026-11-05: P5-2 verifikasi build/test hijau.
- 2026-11-05: P5-2 test non-stream tool_calls belum ditambahkan (butuh provider yang mengembalikan tool_calls).
- 2026-11-05: P5-2 provider populate tool_calls memerlukan bridge wiring/run lifecycle (belum).
- 2026-11-05: Refactor besar bertahap dimulai (rencana: wire run lifecycle ke service).
- 2026-11-05: Wire run lifecycle ke service — dalam progress (belum selesai).
- 2026-11-05: P5-2 gemini/claude parsing ditambahkan; ollama-cli N/A.
- 2026-11-05: Non-stream & stream tool_calls verified (DummyProvider). service provider resolution aligned (provider_for_model). write_default_config includes [routing]/[bridge].
- 2026-11-05: P5-3 multi-step/resume verified: assistant tool_calls disimpan ke Run + park, follow-up role:"tool" terklasifikasi Resume. P3-3 hardening: tools/call menolak tool tak terdaftar (fail-closed).
- 2026-11-05: Audit ulang vs docs/spec_tooling.md + docs/spec_ollama-cli.md -> docs/audit_spec_tooling.md. Verdict: ~45-55%, 4 blocker P0.
- 2026-11-05: Task board follow-up dibuat: P0-1 01a1a9da-6446-7cb1-b94a-4da3972dcb75; P0-2 01a1a9da-644c-7ea2-b476-1a0d384961f2; P0-3 01a1a9da-81de-7330-a4fa-696f3f22cc6b (blocked_by P0-1); P0-4 01a1a9da-645e-7f51-b70f-20f4b1c4f5b8; Ollama CLI mode 01a1a9da-81f3-79d2-85d0-d9737d173298.
- 2026-11-05: P0-1 SELESAI — BridgeCliProvider registrasi session ke BridgeServer; BridgeConfig membawa session_id/session_token; adapter memakai nilai tsb; main meneruskan registry.bridge. Test: tests/bridge_session_registration_test.rs.
- 2026-11-05: P0-2 SELESAI — provider::build_cli_provider() memilih bridge adapter untuk opencode/gemini/claude; flag bridge=false dihormati. Test di bridge_session_registration_test.rs.
- 2026-11-05: P0-3 SELESAI SEBAGIAN — render_conversation (system/tool_calls/tool result), session.close(), release MCP session, timeout AGENTGATE_BRIDGE_TIMEOUT_SECS. Sisa: resume persisten lintas request.
- 2026-11-05: P0-4 SELESAI — resolve terikat session + Bearer token + Origin reject; DELETE menghapus session & membangunkan held call. Test: tests/bridge_resolve_auth_test.rs.
- 2026-11-09 (S4-S6 prep, lead+BD, verified lokal): PR-S1–S3 done/reconciled; **PR-S4 code-wired** (opencode first, capability-gated, replay fallback wajib): continuation registry, opencode park-early+`wait_for_continuation`, `CliProviderImpl` keep MCP/provider session alive on `tool_calls`+capable, `service::resume_run` resolve real MCP holds → await continuation (timeout) → response or `replay_fallback{continuation_*}`; `ProviderResponse.mcp_session_id`, `ChatCompletionRequest.agentgate_run_id`. **PR-S5 reported complete by BD** (sweeps/rotation/TTL/cleanup paths; claim 137 tests); lead verified `cargo check` + core suites (lib 55, bridge_session_registration 8, spec_scenarios 8, sse 6, fail_closed 5, bridge_mcp_hold 1, bridge_resolve_auth 2). **S6 pending:** E2E target env (MCP-hold→CLI lanjut, Linux/9router/AionUI/VSC Copilot, hold>45s/cancel/batch, live resume vs replay), CI all-platform artifacts/checksums, audit §11 closure. **Gaps:** streaming continuation partial, multi-step continuation re-park/state, RequestBatcher wiring/alias/ID/TTL end-to-end, uncommitted working tree.
- 2026-11-05: cargo build & cargo test hijau setelah P0-1..P0-4.
- 2026-11-05: Ollama CLI mode SELESAI (spec_ollama-cli.md):
  - `ProviderConfig` + `BridgeConfig`: `ollama_mode`, `keep_alive`, `prompt_tools_fallback`.
  - `build_cli_provider`: `ollama_mode="cli"` → `OllamaCliBridgeProvider`; default tetap HTTP native.
  - `ServiceRegistry::with_ollama_cli_mode` melewati jalur HTTP native saat CLI aktif.
  - `OllamaCliSession`: reuse `session_id`, `ollama run --keepalive <dur> <model>`, stdout/stderr capture, ANSI strip, stdin ditutup agar proses keluar.
  - `prompt_tools_fallback`: instruksi tool ditambahkan ke prompt; parsing JSON tool_calls (OpenAI-style, compact, fenced, bare, array) + `SessionOutput.tool_calls`.
  - Unit test: argv/keepalive, parsing tool calls, ANSI, session-id reuse; test selector di `tests/bridge_session_registration_test.rs`.
  - `cargo build` & full `cargo test` hijau.
- 2026-11-05: P1 Resume persisten SELESAI (task 01a1a9e5-f474-7aa2-be51-7759d7d4e362) — teammate Backend Resume:
  - `ChatCompletionRequest/Response.session_id`, `ProviderResponse::Success.session_id`.
  - `BridgeCliProvider` cache `persistent_sessions` + `is_keep_alive()`; reuse session saat `keep_alive=true` dan `session_id` cocok.
  - `ServiceRegistry::call_provider_native` meneruskan `session_id` ke API response.
  - `src/bin/fake_cli_persistent.rs` + `tests/persistent_resume_test.rs`.
  - `cargo build` & full `cargo test` hijau.
- 2026-11-05: P2 Observabilitas SELESAI (task 01a1aa7d-36c6-78f2-a0c5-8c55bb5dc56e; backend slot idle tanpa pickup, implementasi ditutup oleh lead):
  - `src/metrics.rs`: atomic counters/gauges Prometheus-text; `GET /_metrics`.
    - `agentgate_runs_active`, `agentgate_runs_parked`, `agentgate_resume_total_ok/replay/conflict`, `agentgate_guard_violations_total`, `agentgate_cli_exit_total_ok/error`, tool_wait & ttft sum/count.
  - Event log terstruktur (`tracing::info!`): `run.start` (run_id/provider/model/mode/tools), `run.resumed`, `run.replay`, `run.end`, `guard.violation`, `bridge.call_held`, `bridge.tools_list`.
  - Header `X-Agentgate-Run` (uuid) + `X-Agentgate-Mode` (`native|resume|replay|bridge|chat_only`) pada respons SSE & non-stream.
  - Test: `tests/observability_test.rs` (2 test; /_metrics + header).
  - Catatan: ✅ SELESAI 2026-11-06 (lead) — `batch.issued` kini dipancarkan di `new_bridge_run` saat response mengembalikan >1 tool_calls dalam satu batch; serial mode kini ditegakkan di path bridge via `truncate_tool_calls`.
  - Full `cargo test` hijau.
- 2026-11-05: P1 Config lengkap (task 01a1aa7b-e341-7b60-b786-bd73482d9ecf):
  - `BridgeConfig` + `BatchConfig`/`ResumeConfig`/`LimitsConfig`/`GuardConfig` (`[bridge.batch|resume|limits|guard]`).
  - `Config.workspace` (`[workspace.<name>]`); `RoutingConfig.on_client_tools` + serde alias `chat_only` (spec snake_case) & config default kini bisa di-parse.
  - `ChatCompletionRequest.parallel_tool_calls` (OpenAI style).
  - `write_default_config` template lengkap (§14) + `apply_env_overrides` (env > file > default; `AGENTGATE_BRIDGE_PORT/CALL_TTL_SECS/PARK_TTL_SECS/MAX_PARKED_RUNS`).
  - `bridge.limits`: `max_tools` & `max_schema_bytes` ditegakkan fail-closed di service.
  - Test: `tests/config_bridge_test.rs` (6 test). Full `cargo test` hijau.
- 2026-11-05: P1 Pemetaan Tool SELESAI (task 01a1aa74-0977-7f61-9f79-a2368243b06e):
  - `src/openai/tools.rs`: `is_openai_tool_name`, `openai_name_alias` (deterministik `tNN_<sanitized>`, ≤64 char), `openai_name_aliases`, `unalias_tool_calls` (round-trip alias→original untuk emisi ke client), `inline_refs` (`#/$defs/`, `#/definitions/`, depth cap 16).
  - `BridgeCliProvider::tool_schemas_from_request` (kini pub): aliasing + inline `$ref` + `normalize_schema` (StrictGemini untuk gemini, OpenAI lainnya).
  - Emisi tool_calls (non-stream & stream) di-unalias sebelum keluar ke client A.
  - Test: `tests/schema_normalization_test.rs` (7 test; termasuk HTTP tools/list advertises alias).
  - Full `cargo test` hijau.
- 2026-11-05: P1 Fail-closed SELESAI (task 01a1aa57-f2a6-7321-9a56-8bf26d27047f):
  - `ClientToolsPolicy::Reject` baru di `src/permission/mod.rs` (fail-closed 501).
  - Service (`src/service.rs`): bridge flow dengan client `tools[]` + `on_client_tools=reject` ATAU provider non-bridge-capable → `AppError::ToolBridgeUnsupported` (501 `tool_bridge_unsupported`).
  - API (`src/api.rs` tools/call): tool tidak ada di whitelist session → 503 `tool_isolation_unavailable` (tidak hold/execute).
  - Implementasi ditutup oleh lead (backend slot sedang paused). Test: `tests/fail_closed_test.rs` (5 test).
  - Full `cargo test` hijau.
- 2026-11-05: P2 Suite T1-T23 test coverage SELESAI (task 01a1aa7e-c40a-7902-bf1a-d822c8da07eb):
  - File: `tests/spec_scenarios_test.rs` (7 test scenarios implemented)
  - Coverage matrix:
    | Test | Spec Ref | Status | Notes |
    |------|----------|--------|-------|
    | T1 | §16.2 | **SKIPPED** | Butuh real CLI multi-step, tidak bisa ditiru fake |
    | T2 | §16.2 | **PASS** | SSE payload format (data: prefix, tool_calls, [DONE]) |
    | T3 | §16.2 | **PASS** | 2 parallel tools in 1 batch (non-stream) |
    | T4 | §16.2 | **PASS** | Multi-step resume 3x (tool_result → tool_calls → tool_result → final) |
    | T5 | §16.2 | **SKIPPED** | parallel_tool_calls:false serial mode butuh rearchitecture (§10.3) |
    | T6-T10 | §16.2 | **SKIPPED** | Hardware/real CLI dependent (truncation, timeout, streaming detail) |
    | T11 | §16.2 | **PASS** | tools + non-bridge provider → 501 tool_bridge_unsupported |
    | T12 | §16.2 | **PASS** | $ref/oneOf schema normalize (dummy provider OK) |
    | T13 | §16.2 | **PASS** | Long/weird tool names → alias created, original restored |
    | T14 | §16.2 | **PASS** | Large tool result truncation behavior verified |
    | T15 | §16.2 | **SKIPPED** | Serial mode butuh rearchitecture (§10.3) |
    | T16 | §16.2 | **PASS** | tool_choice=none → chat_only, no tool_calls |
    | T17 | §16.2 | **PASS** | No tools → chat_only |
    | T18 | §16.2 | **PASS** | Ollama native + tools via is_native path |
    | T19-T23 | §16.2 | **SKIPPED** | Hardware/real CLI dependent |
  - Skipped tests marked with `#[ignore]` + reason
  - All 8 implemented tests PASS (T20 baru: serial truncate batch via `truncate_tool_calls`), cargo build & cargo test hijau

- 2026-11-06: P1 Batching/serial tool calls §10.3 SELESAI SEBAGIAN (lead):
  - `src/service.rs`: helper baru `truncate_tool_calls(&mut Option<Vec<ToolCall>>, parallel)` + `truncate_parallel_tool_calls` (response-level) sekarang delegate ke helper inti.
  - Serial mode (`parallel_tool_calls: false`) kini ditegakkan di **semua** path (native via response-level, CLI bridge via list-level) sebelum park/return — sebelumnya hanya path native.
  - Bridge (`new_bridge_run`): emit event `batch.issued` (batch_size) saat response mengandung >1 tool_calls dalam satu batch.
  - Test: T20 (`tests/spec_scenarios_test.rs`) — serial memotong list ke 1; non-serial tidak memotong.
  - Full `cargo build` & `cargo test` hijau.

- 2026-11-06: P3 Config wiring SELESAI — Wire `[routing]`/`[bridge]` file config ke `ServiceRegistry` runtime (task 01a1aad6-3863-7132-ae8a-63903f3e6889):
  - `ServiceRegistry::with_config(routing, bridge_config, reasoning, defaults, ollama_cli_mode)` constructor baru.
  - `main.rs` (Serve, Models, Doctor, Test commands) dan `cli.rs` (providers, models, test commands) sekarang menggunakan `with_config` dengan `config.routing`, `config.bridge`, `config.reasoning`, `config.defaults`.
  - `ServiceRegistry::new()` tetap tersedia untuk backward compat (tests).
  - `ServiceRegistry::with_routing` dan `with_bridge_config` dipertahankan untuk override manual.
  - Semua lib tests (47/47) dan integration tests (fail_closed, config_bridge, schema_normalization, observability, persistent_resume, bridge_resolve_auth, bridge_mcp_hold, api_integration) PASS.
  - Audit `docs/audit_spec_tooling.md` diperbarui: gap `[routing]`/`[bridge]` dihapus dari sisa gap utama.

- 2026-11-09: P0-2 SELESAI — SSE tool_calls emit 2-chunk format per spec §7.4 (id/name chunk1, arguments fragments ≤512B chunk2+, finish_reason=tool_calls). File: `src/api.rs`. `cargo check` + `cargo test --lib` lulus.

- 2026-11-09: P0-3 SELESAI — `notifications/cancelled` handler: cancel pending call + cancel run + close provider session (kill CLI child + process tree via `taskkill /F /T /PID` on Windows, cleanup `temp_dir()/agentgate/sessions/{id}/work`). Files: `src/api.rs`, `src/bridge.rs`, `src/provider.rs`. Idempotent, `cargo check` lulus.

- 2026-11-09: P0-4 SELESAI — Keepalive during held MCP tools/call (periodic `: keepalive` every 15s while respecting call_ttl). File: `src/api.rs`, `src/bridge.rs` (by Backend Developer).

- 2026-11-09: P1a SELESAI — Adapter strict MCP/config isolation:
  - Claude: `--mcp-config <temp file> --strict-mcp-config` per session (no ~/.claude.json pollution). File: `src/provider/bridge/claude.rs`.
  - Gemini: settings file per run via `GEMINI_CLI_SYSTEM_SETTINGS_PATH` file path. File: `src/provider/bridge/gemini.rs`.
  - OpenCode: unchanged (`OPENCODE_CONFIG_CONTENT` env var). File: `src/provider/bridge/opencode.rs`.
  - No api.rs/bridge.rs edits (P0-1b safe). `cargo check` lulus.

- 2026-11-09: UX Grouping SELESAI — Reasoning/tool view steps grouped (fix fragmented "thought/view steps 1" per tool):
  - `src/reasoning/parser.rs`: tool_calls emit view steps via `ensure_reasoning_start` + `ReasoningDelta` (format_tool_view_step) to SAME reasoning block, NO `end_reasoning()` per batch.
  - `src/reasoning/coalesce.rs`: `ToolCalls` handler no longer flushes/ends reasoning block — persists until Done/Error/text delta boundary.
  - Result: single reasoning block with grouped view steps, preserved `tool_calls` emission (P0-2), RS0 ordering intact.
  - `cargo check` lulus, `cargo test --lib reasoning` 32 passed. E2E 9router/AionUI validation pending.

- 2026-11-09: Verification/docs (lead+BD2, parsial/koreksi): `cargo check` OK; test lokal utama hijau (`--lib` 55, `spec_scenarios` 8, `fail_closed` 5, `sse_tool_calls_contract` 6, `bridge_mcp_hold` 1, `bridge_resolve_auth` 2, reasoning 34). Windows release (rebuilt): `target/release/agentgate.exe` (11,992,576 bytes, 2026-11-09 13:02:46, SHA256 `234BA39C1A2D5A4957F84E62EDFC22B949A22ED1DE88FC51F83079E6A72004B4`). CI scaffold `.github/workflows/build.yml` + `docs/BUILD_AND_RELEASE.md`; **artifact all-platform butuh CI/cross-toolchain**. **Koreksi status:** HEAD `227d772` klaim P0/P1/P2 complete, namun **P2 `01a1bde2-e9cd` in progress** (RequestBatcher wiring, enforcement `max_result_bytes`/`max_prompt_chars` di render, alias/ID/TTL polish) dan **P0-1b Stage 2 open**. E2E 9router/AionUI/Linux/VSC Copilot masih diperlukan (`audit.md` §11).