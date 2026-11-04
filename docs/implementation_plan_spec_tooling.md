# Implementation Plan — SPEC_TOOLING.md (MCP Bridge)

Status: Plan v0.1 · Tanggal: 2026-11-03 · Basis: `docs/spec_tooling.md` (Draft v0.2) + `docs/audit_spec_tooling.md`

## 0. Tujuan & Prinsip

- Implementasi penuh spesifikasi MCP Bridge (`spec_tooling.md`) sehingga tool yang diminta model dieksekusi **hanya oleh client (A)**, tidak pernah oleh server B.
- Fail-closed: bila isolasi tool tidak bisa dijamin → tolak request (503/501).
- Setiap fase harus lolos `cargo build` + `cargo test` sebelum lanjut.

## 1. Kondisi Saat Ini (ringkas)

- API proxy OpenAI→CLI berfungsi (chat non/stream, /v1/models, /health).
- Modul `bridge`, `run`, `openai`, `permission`, `provider/*` sudah ada sebagai **scaffolding** tapi **tidak ter-wire di `lib.rs`** dan **tidak kompil** (25+ error).
- `ChatCompletionRequest` sudah punya `tools`/`tool_choice`/`parallel_tool_calls`, tetapi tidak dipakai.
- Tidak ada classify, MCP endpoint, run lifecycle, disable builtin tools, guard, error codes, streaming tool_calls.

## 2. Tim & Peran

| Role | Tanggung jawab utama |
|---|---|
| Backend | Axum routes, MCP endpoint, run registry, provider adapters, streaming |
| Features | Schema normalization, tool mapping, alias, classifier, permission policy |
| DevOps | Config (bridge/routing sections), security (bind loopback, auth, CORS), CI/build, launch |
| QA | Unit/integration test, guard test, smoke test end-to-end |

## 3. Fase & Task Breakdown

### Phase 0 — Fondasi (Backend) — blocker semua fase
- P0-1: Wire modul di `lib.rs` (`bridge`, `run`, `openai`, `permission`) dan perbaiki compile errors (Config Default, ChatMessage fields, ChatCompletionChunk.usage, rand crate, ProviderRegistry).  
  *Est*: 2 hari · *Owner*: Backend
- P0-2: Tambah dependency `rand` ke Cargo.toml (atau ganti dengan uuid).  
  *Est*: 0.5 hari
- P0-3: Tambah `#[derive(PartialEq)]` pada ChatMessage + impl `tool_result` helper.  
  *Est*: 0.5 hari
- P0-4: Tambah field `bridge`/`routing` ke Config (struct + Default).  
  *Est*: 0.5 hari

**Exit criteria**: `cargo build` & `cargo test` hijau.

### Phase 1 — Types & Config (Backend + DevOps)
- P1-1: Extend `Config` dengan `BridgeConfig` (port, call_ttl_secs, park_ttl_secs, max_parked_runs, max_steps, transport) dan `RoutingConfig` (no_tools_policy, on_client_tools).  
  *Ref*: spec §9.1, §6, §5
- P1-2: Extend `ChatCompletionChunk`/`Delta` dengan field `tool_calls` (streaming tool calls).  
  *Ref*: spec §7.4
- P1-3: Extend `AppError` dengan kode spec (tool_bridge_unsupported 501, tool_isolation_unavailable 503, tool_run_expired 409, too_many_parked_runs 429, unsupported_tool_type 400, provider_error 502, tool_isolation_violation 502) + field `code`.  
  *Ref*: spec §7.6
- P1-4: Config file default template di `write_default_config` (bridge/routing section).  
  *Owner*: DevOps

### Phase 2 — Classification & Mode (Features + Backend)
- P2-1: Implement `classify()` per spec §7.2 (Native/Resume/Replay/NewBridge/NewChatOnly).  
  *Ref*: spec §7.2 `trailing_tool_messages`
- P2-2: Implement `determine_tool_mode()` decision table §6.1.  
- P2-3: `tool_choice` handling §8.5 (auto/none/required/specific function/parallel).  
- P2-4: `n>1` → 400, unsupported tool type → 400, response_format handling.  
  *Owner*: Features (logic) + Backend (HTTP)

### Phase 3 — MCP Bridge Server (Backend)
- P3-1: HTTP endpoint `POST /_bridge/mcp/{session_id}` (loopback only, separate listener).  
  *Ref*: spec §9.1
- P3-2: Auth Bearer token (constant-time compare), reject Origin header, 404 unknown session.  
  *Ref*: spec §9.2
- P3-3: JSON-RPC methods: initialize, notifications/initialized, ping, tools/list, tools/call, notifications/cancelled, resources/prompts stub.  
  *Ref*: spec §9.3
- P3-4: Hold `tools/call` (pending call + oneshot, SSE keepalive 15s, timeout call_ttl).  
  *Ref*: spec §9.5
- P3-5: stdio shim fallback (opsional, v1.1).  
  *Ref*: spec §9.7

### Phase 4 — Run Lifecycle & Registry (Backend)
- P4-1: State machine Run (Starting/Running/AwaitingTools/Resuming/Done/Expired/Cancelled/Failed/Tainted).  
  *Ref*: spec §10.1
- P4-2: RunRegistry: create/park/resume/cancel, max_parked_runs, TTL cleanup task.  
- P4-3: Resume by tool_call_id (lookup parked run); Replay fallback (rotate session) bila id hilang.  
  *Ref*: spec §10.4, §10.5
- P4-4: Tainted session on run failure.  
  *Ref*: spec §10.9

### Phase 5 — Provider Adapters: disable builtin tools (Backend + Features)
- P5-1: opencode: inject `OPENCODE_CONFIG_CONTENT` dengan tools/permission dimatikan + MCP remote config. ⚠️ verifikasi versi CLI.  
- P5-2: gemini: settings sistem/per-run (exclude/core tools) + MCP httpUrl.  
- P5-3: claude: `--mcp-config` + `--strict-mcp-config` + `--tools ""`.  
- P5-4: ollama: native proxy (model-api), pass-through tools/tool_calls.  
  *Ref*: spec §5, §12.5

### Phase 6 — Schema Normalization & Tool Mapping (Features)
- P6-1: OpenAI function → MCP tool mapping (parameters→inputSchema, drop strict).  
  *Ref*: spec §8.1
- P6-2: Normalisasi skema (root object, remove $schema, inline $ref depth 8, size 64KB, max 128 tools).  
- P6-3: Profil permissive (opencode/claude) & strict_gemini (buang additionalProperties/patternProperties/oneOf/anyOf/allOf, format unknown, enum campuran→string).  
- P6-4: Alias & penamaan (budget per prefix, regex, t{NN}_..., NameMap per Run, duplikat sufiks).  
  *Ref*: spec §8.3
- P6-5: tool_call_id format `call_`+16 hex (8 run + 2 batch + 2 idx + 4 random).  
  *Ref*: spec §8.4
- P6-6: Tool result → MCP result (text array, isError heuristic, truncation 256KB).  
  *Ref*: spec §8.6
- P6-7: Tool tambahan dari perantara (headroom) — perlakukan sama.  
  *Ref*: spec §8.7

### Phase 7 — Streaming tool_calls (Backend)
- P7-1: SSE chunk sequence §7.4 (role/content/tool_calls index/id/name/arguments/finish_reason).  
- P7-2: arguments dalam satu chunk atau pecah ≤512 byte UTF-8.  
- P7-3: Usage chunk sebelum [DONE] bila include_usage.  
- P7-4: Error setelah stream dimulai → chunk error + [DONE].  
  *Ref*: spec §7.4, §7.6

### Phase 8 — Guards & Error Codes (Backend + Features)
- P8-1: Guard: CLI event menunjukkan tool non-bridge → hentikan run (tool_isolation_violation 502).  
  *Ref*: spec §2.3.4, §7.6
- P8-2: on_client_tools policy (bridge/passthrough/ignore) + reject mode.  
- P8-3: Fail-closed bila isolasi tidak bisa dijamin (503).  

### Phase 9 — ollama Native (Backend)
- P9-1: Provider class model-api detection; proxy HTTP ke Ollama; pass tools/tool_calls native.  
  *Ref*: spec §5, §12.5

### Phase 10 — Security & Config (DevOps)
- P10-1: Bridge bind hanya loopback, port acak/tetap via bridge.port.  
- P10-2: Token 256-bit per Run, constant-time compare, never log.  
- P10-3: CORS, bearer auth, bind warning (sudah ada, verifikasi).  
- P10-4: CI: cargo build/test/clippy; release profile.

### Phase 11 — Testing (QA)
- P11-1: Unit test classify, schema normalization, alias, tool_call_id.  
- P11-2: Integration test MCP initialize/tools/list/tools/call hold.  
- P11-3: Integration test resume/replay, parked run TTL.  
- P11-4: Guard test (non-bridge tool → violation).  
- P11-5: E2E smoke: AionUi→9router→agentgate→CLI (manual).

### Phase 12 — Launch (DevOps)
- P12-1: README + quickstart.  
- P12-2: Version bump, changelog.  
- P12-3: Tag & release binary.

## 4. Dependency Graph (ringkas)

```
P0 (fondasi) → P1 (types/config) → P2 (classify) → P3 (MCP) → P4 (run) → P5 (providers) → P6 (schema) → P7 (stream) → P8 (guards) → P9 (ollama) → P10 (security) → P11 (tests) → P12 (launch)
```

P6 dapat paralel dengan P3/P4. P9 paralel dengan P7.

## 5. Risiko (R) — dari spec §19

| R | Risiko | Mitigasi |
|---|---|---|
| R1 | CLI versi berubah, flag/config tidak dikenal | Spike per provider dulu (P5), verifikasi ⚠️ |
| R2 | MCP transport mismatch (HTTP vs stdio) | stdio shim fallback P3-5 |
| R3 | Tool name prefix/length limit model | Alias P6-4 |
| R4 | Koneksi HTTP tools/call timeout di proxy/CLI | SSE keepalive P3-4 |
| R5 | 9router/Headroom menambah tool & compress | P6-7, §8.7 |
| R6 | Parked run hilang (restart) → replay fallback | P4-3 |
| R7 | Tool result besar | truncation P6-6 |
| R8 | Concurrency/limit | max_parked_runs, max_steps |
| R9 | Windows path/encoding | test di Windows (env saat ini) |

## 6. Estimasi Total

- Phase 0-4 (core bridge): ~2 minggu (Backend 1 orang)
- Phase 5-9 (providers+stream+guards+ollama): ~2 minggu
- Phase 10-12 (security/tests/launch): ~1 minggu
- **Total ≈ 5 minggu** untuk v1 bridge siap.

## 7. Langkah Berikutnya (sekarang)

1. Backend: eksekusi Phase 0 (P0-1..P0-4) agar build hijau.
2. Features: mulai P6-1..P6-3 (schema) paralel karena murni logic.
3. DevOps: P1-4 + P10-1..P10-3.
4. QA: siapkan test skeleton P11-1.

## 8. Catatan

- Jangan implementasi tool execution di B (G1). Semua tool bawaan CLI harus dimatikan saat bridge aktif (chat_only default).
- Default `routing.no_tools_policy = "chat_only"` (bukan passthrough) untuk skenario AionUi→9router→agentgate.
- Semua perubahan config CLI disuntik per-proses, tidak mengubah file config pengguna (prinsip §2.3.3).
