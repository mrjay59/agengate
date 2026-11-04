# Task Breakdown — Implementasi spec_tooling.md (agentgate)

Tanggal: 2026-11-03 · Status: P0 (types/config/wiring) selesai; P1–P3 tersisa.

## 0. Status Saat Ini
- Build & unit tests hijau (`cargo build`, `cargo test`).
- Modul bridge/run/openai/permission sudah ter-wire di `lib.rs` dan kompilasi.
- `ChatCompletionRequest` sudah punya `tools`, `tool_choice`, `parallel_tool_calls`, `response_format`.
- `Config` punya `routing` (NoToolsPolicy/ClientToolsPolicy) dan `bridge` (BridgeConfig).
- `ChatMessage`/`Message` punya `tool_calls`, `tool_call_id`, `name`.
- **Belum**: MCP endpoint, classify/mode selection, disable builtin tools injection, streaming tool_calls §7.4, error codes, TTL, ollama passthrough, guard.

## 1. Fase & Milestone
| Fase | Scope | Milestone |
|---|---|---|
| P0 (done) | Types, config, wiring, compile green | M0 |
| P1 | Classify + mode selection + chat_only default + disable builtin tools | M1 |
| P2 | MCP bridge endpoint + hold tools/call + park/resume | M2 |
| P3 | Streaming tool_calls §7.4, error codes §7.6, TTL cleanup, guard | M3 |
| P4 | Ollama native passthrough, preflight, docs | M4 |

## 2. Task per Role

### Backend (api/service/routing)
- **B1** Implement `classify` + `Classified` enum di `src/run.rs` (§7.2). Dep: P0. Est: 3d.
- **B2** Mode selection `determine_tool_mode` terhubung ke `api.rs` (§6.1). Est: 2d.
- **B3** Default `no_tools_policy=chat_only`: jangan kirim tools ke CLI; cwd kosong. Est: 1d.
- **B4** Error codes §7.6 (501/503/409/429/400/502) di `error.rs` + handler. Est: 2d.
- **B5** `n>1` reject + unsupported tool type 400 (done di api.rs). Est: 0.5d ✅.
- **B6** Handler `/v1/chat/completions` streaming SSE §7.4 (tool_calls chunk). Est: 4d.

### Features (bridge/MCP/run lifecycle)
- **F1** MCP endpoint `POST /_bridge/mcp/{session}` di `src/api.rs` + auth Bearer constant-time + Origin reject (§9.1-9.2). Est: 4d.
- **F2** `initialize`, `tools/list`, `tools/call` hold + keepalive 15s (§9.3-9.5). Est: 5d.
- **F3** Run state machine + park/resume/replay (§10.1-10.5). Est: 6d.
- **F4** Tool mapping: normalize_schema per profile, NameMap alias (§8.2-8.4). Est: 3d.
- **F5** Tool result → MCP result (§8.6) + size cap 256KB. Est: 2d.
- **F6** TTL cleanup task + max_parked_runs (§10.6). Est: 2d.
- **F7** Guard: deteksi tool non-bridge → kill Run + 502 (§13). Est: 3d.

### Provider adapters (disable builtin tools)
- **P1** OpenCode: inject `OPENCODE_CONFIG_CONTENT` dengan tools/permission off (§4.3). Est: 2d.
- **P2** Gemini: settings file + exclude builtins (§4.3). Est: 2d.
- **P3** Claude: `--mcp-config` + `--strict-mcp-config` + `--tools ""` (sudah di profile). Est: 1d.
- **P4** Ollama native passthrough (§12.5). Est: 3d.

### DevOps
- **D1** Config template update (routing/bridge/provider/ollama) di `write_default_config`. Est: 1d.
- **D2** Bridge loopback listener (port acak) terpisah dari publik. Est: 1d.
- **D3** Preflight probe + version check. Est: 2d.
- **D4** Integration test end-to-end tool flow (mock client). Est: 3d.

## 3. Dependency Graph
```
P0 (done) → B1 → B2 → B3 → B6
P0 → F1 → F2 → F3 → F5 → F6
F3 → F7
P1/P2/P3 → F2
P4 → B4
D1 → all
```

## 4. Risk & Mitigation
- **R1** Tool bocor ke B → mitigasi: default chat_only + reject on_client_tools (sudah).
- **R2** Dead code tidak kompilasi → mitigasi: P0 wiring selesai; sisanya inkremental.
- **R3** MCP HTTP incompatibility dengan CLI → fallback stdio shim (§9.7).
- **R4** Parked run hilang → replay fallback (§10.5).

## 5. Definition of Done per Fase
- P1: request tanpa tools → CLI jalan tanpa tool bawaan; request dengan tools → 501 reject (sebelum bridge).
- P2: tools/call ditahan dan diteruskan sebagai tool_calls ke client; resume berfungsi.
- P3: streaming §7.4, error codes, TTL, guard hijau.
- P4: ollama native, preflight, docs.

## 6. Catatan untuk Tim
- Jangan ubah `on_client_tools` default dari `bridge` tanpa review.
- Semua mutasi Run lewat satu task (channel), hindari lock lintas `.await`.
- Token bridge tidak boleh dicatat di log.
- Gunakan `cargo test` + `cargo build` sebelum PR.