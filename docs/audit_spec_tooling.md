# Audit: agentgate vs docs/spec_tooling.md

Tanggal audit: 2026-11-03  
Scope: `C:\Users\EDN\Documents\AION\project\agentgate` (src/, Cargo.toml, docs/)

## 0. Ringkasan
Implementasi **aktif** (yang benar-benar dikompilasi) adalah proxy OpenAI sederhana
ke CLI (`service.rs` + `provider.rs` + `api.rs`). Seluruh modul "bridge" yang ada di
repo (`src/bridge.rs`, `src/run.rs`, `src/openai/*`, `src/permission/*`,
`src/provider/*.rs`) **tidak dideklarasikan di `lib.rs`** → dead code, dan sebagian
besar tidak akan kompilasi jika diaktifkan (referensi tipe yang tidak ada, mis.
`types::Message`, `provider::ProviderRegistry`).

Kesimpulan: **belum sesuai** dengan `spec_tooling.md`. Sebagian besar requirement
§4–§13 (bridge, run lifecycle, mode selection, tool mapping, MCP server, guard)
belum terimplementasi pada jalur eksekusi.

## 1. Modul & Wiring
| Modul spec | File repo | Status | Catatan |
|---|---|---|---|
| `server/routes_chat.rs` | `src/api.rs` | ⚠️ | Hanya `/v1/chat/completions`, `/v1/models`, `/health`. Tidak ada classify/mode. |
| `openai/*` | `src/openai/tools.rs`, `sse_tools.rs` | ❌ dead | Tidak dideklarasikan di `lib.rs`; tidak dikompilasi. |
| `bridge/*` | `src/bridge.rs` | ❌ dead | Tidak dideklarasikan; referensi `types::Message` (tidak ada). |
| `provider/*` | `src/provider.rs` (aktif) + `src/provider/*.rs` (dead) | ⚠️ | Aktif hanya `CliProviderImpl` sederhana; adapter kaya (opencode/gemini/claude/ollama) dead. |
| `permission/*` | `src/permission/mod.rs` | ❌ dead | Enums ada tapi tidak dipakai. |

## 2. Kontrak API (§7)
- `ChatCompletionRequest` (`src/types.rs`) **tidak** memiliki `tools`, `tool_choice`,
  `parallel_tool_calls`, `response_format`, `n` handling → ❌ §7.1.
- `classify` / `Classified` (§7.2) tidak ada → ❌.
- Respons non-stream dengan `tool_calls` (§7.3) tidak dihasilkan → ❌.
- Respons stream format §7.4 tidak dihasilkan (api.rs mem-buffer seluruh jawaban
  lalu emit 1 chunk content + 1 chunk finish) → ❌.
- Error codes §7.6 tidak ada → ❌.

## 3. Pemilihan Mode (§6)
- `classify_request`/`determine_tool_mode` ada di `src/run.rs` (dead) → ❌ pada jalur aktif.
- Default `no_tools_policy = "chat_only"` tidak dikonfigurasi (config tidak punya
  section routing) → ❌.

## 4. Tool Mapping (§8)
- Normalisasi skema (`normalize_schema`) ada di `src/openai/tools.rs` (dead) → ❌ aktif.
- NameMap/alias ada (dead) → ❌ aktif.
- `tool_call_id` format `call_`+16 hex tidak digunakan (generator memakai prefix
  `call_...` berbeda) → ⚠️.
- Tool result → MCP result (§8.6) tidak ada → ❌.

## 5. Bridge MCP Server (§9)
- Endpoint `/_bridge/mcp/{session}` tidak ada → ❌.
- Auth Bearer constant-time, Origin reject, session TTL → ❌.
- Hold `tools/call` + keepalive → ❌.

## 6. Run Lifecycle & Registry (§10)
- State machine ada di `src/bridge.rs`/`src/run.rs` (dead) → ❌ aktif.
- Parked runs, resume, replay → ❌ aktif.
- TTL cleanup task tidak dijalankan → ❌.

## 7. Provider Adapters & Disabling Builtin Tools (§4.3, §5)
- `opencode`/`gemini`/`claude` adapter di `src/provider/*.rs` (dead) → ❌ aktif.
- Tidak ada injeksi config untuk mematikan tool bawaan (OPENCODE_CONFIG_CONTENT,
  --mcp-config, --strict-mcp-config, --tools "") → ❌.
- `ollama` native provider ada (dead) → ❌ aktif.

## 8. Security / Guard (§13)
- Tidak ada deteksi tool non-bridge → ❌.
- Fail-closed 503 `tool_isolation_unavailable` → ❌.

## 9. Yang Sudah Sesuai (parsial)
- Struktur dasar OpenAI-compatible (`/v1/models`, `/v1/chat/completions`, `/health`) ✅.
- Provider discovery via `models_cmd`/`chat_cmd` ✅ (sederhana).
- ANSI stripping, JSONL parsing untuk opencode ✅ (sederhana).
- CORS + Bearer auth middleware ✅.

## 10. Rekomendasi Prioritas
1. **P0** Wire modul `bridge`, `run`, `openai`, `permission` ke `lib.rs` dan perbaiki
   compile errors (tipe `Message`, `ProviderRegistry`) agar dead code menjadi live.
2. **P0** Tambahkan field `tools`/`tool_choice` ke `ChatCompletionRequest`.
3. **P1** Implement classify + mode selection di `api.rs`/`service.rs`.
4. **P1** Implement MCP bridge endpoint + hold `tools/call`.
5. **P1** Disable builtin CLI tools via per-run config injection.
6. **P2** Streaming tool_calls sesuai §7.4, error codes §7.6, TTL cleanup.
7. **P2** Ollama native passthrough.

## 11. Kesimpulan
Status kesesuaian terhadap `spec_tooling.md`: **~15% (fondasi API saja)**.
Implementasi bridge/run/MCP belum aktif. Perlu pekerjaan signifikan sebelum
memenuhi spec.
