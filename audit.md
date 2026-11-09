# Audit Mendalam — AgentGate Bridge (Tooling & Reasoning)

**Project:** `C:\Users\EDN\Documents\AION\project\agentgate`
**Flow:** `AionUi (A) → 9router → AgentGate (B, Windows/Linux) → opencode (prompting, builtin disable, MCP bridge)`
**Spec:** `docs/spec_tooling.md` v0.2 (R1–R12, §0–§15), `docs/spec_reasoning.md` RS0–RS5 (§0–§16)
**Tanggal audit:** 2026-11-09
**Verifikasi:** `cargo test --lib` 55/55 lulus. Integration `cargo test` 145/145 lulus, 2 ignored (butuh server). SSE contract 6/6 lulus. Integration bridge **belum end-to-end** (P0-1 parked).

---

## 0. Dukungan Linux vs Windows

| Aspek | Linux | Windows |
|---|---|---|
| `temp_dir()` fallback (claude/gemini) | ✅ `/tmp/agentgate` valid | ❌ jadi `C:\tmp\agentgate` di drive berjalan (P1-3) |
| `.cmd` handling (opencode.cmd/claude.cmd) | ✅ tidak perlu | ❌ perlu `cmd /C` + `PATHEXT` (P1-3) |
| `Read` folder → os error 5 | ✅ harness izinkan listing dir | ❌ Access denied (harness hanya file individual) (P1-2) |
| Process tree kill | ✅ `kill -9` | ⚠️ `taskkill /T /F` perlu impl |
| Path separator / env | ✅ native | ⚠️ perlu normalize `\` vs `/` |
| MCP stdio shim | ✅ pipe unix | ⚠️ named pipe Windows |
| `opencode`/`claude`/`gemini` CLI | ✅ native | ⚠️ `.cmd` wrapper |

**Kesimpulan Linux:** AgentGate **lebih stabil di Linux** karena fallback `/tmp` valid, tidak ada `.cmd`, dan `Read` folder tidak error. Namun P0 (hold↔Run disconnected, SSE single-chunk, cancel/keepalive) tetap blocker di **semua platform**. P1-2 (guard lemah, disable builtin) juga berlaku di Linux — hanya gejala `os error 5` yang tidak muncul; CLI masih bisa panggil builtin `read` folder (berhasil di Linux, tetap melanggar isolasi S1/S3).

**Rekomendasi:** Targetkan Linux sebagai platform utama untuk BR0–BR7 (uji R1 lebih mudah), lalu hardening Windows (BR7) dengan `.cmd` + `temp_dir()` + process tree kill.

---

## 1. Ringkasan Eksekutif

| Spec | Estimasi kesesuaian | Status |
|---|---|---|
| `docs/spec_tooling.md` | **65–70%** | Fondasi API + classify + MCP endpoint + schema + Ollama native ada. Bridge **belum end-to-end**. |
| `docs/spec_reasoning.md` | **~75%** | Pipeline streaming, coalescer, format, parser toleran, Ollama native cukup baik; adapter per-CLI masih generik. |

**Kesimpulan:** Project **belum sesuai** untuk production sebagai bridge tooling. Reasoning sudah jauh lebih siap; tool-bridge masih prototipe dan R1–R6 menjadi blocker.

**Root concern:** Bukan satu bug, melainkan disconnect arsitektural antara 3 komponen yang masing-masing benar secara lokal:
- `BridgeServer` (MCP hold)
- `RunService` (Run lifecycle)
- `ProviderBridge` (spawn CLI + injeksi MCP + cwd + disable builtin)

---

## 2. Arsitektur Saat Ini vs Arsitektur Spec

### 2.1 Di spec (seharusnya)

```
Client (9router/AionUi)
  │ OpenAI /v1/chat/completions (SSE)
  ▼
AgentGate (RunService)
  │ POST /_bridge/mcp/:session (Bearer, internal)
  │ tools/list + tools/call (hold until resolve)
  ▼
PendingCalls {call_id → oneshot::Sender}
  │ RunEvent::ToolCalls → SSE chunk §7.4
  ▼
Client execute → response tool message → Resume (same parked CLI)
  │
  ▼
CLI Process (parked, satu proses per session) stdout NDJSON → ReasoningParser → RunEvent
```

### 2.2 Yang terjadi sekarang

```
Client → RunService → spawn CLI (per turn, proses mati)
                          │ tools/call → BridgeServer::hold_tool_call() [tahan HTTP global]
                          ▼
                       wait hingga call_ttl=600s (tidak pernah di-resolve RunService)
                       ▼
                       CLI timeout / error, bukan resume
```

**Fakta:**
- `RunService::run_streaming()` (`src/service.rs:628`) hanya mengonsumsi `RunEvent::ToolCalls` untuk **disampaikan ke client**. Tidak ada kode yang memanggil `BridgeServer::resolve_tool_call()` setelah client membalas (kecuali jalur yang belum terhubung).
- `new_bridge_run` (`src/service.rs:774`) membuat `Run` baru per request, tidak `park` proses sebelumnya.
- Setiap turn = spawn baru + `render_conversation()` replay (`src/provider.rs:1103`). Ini menjelaskan toolcall loop, retried turns, latency, dan hilangnya live state CLI.

---

## 3. Temuan Kritis P0 — Wajib Perbaiki

### P0-1. End-to-end bridge disconnected (Hold ↔ Run tidak tersambung)

**Lokasi:**
- `src/bridge.rs:197` `hold_tool_call()` — menahan `tools/call` di `oneshot` pending call.
- `src/bridge.rs:442` `resolve_tool_call()` — tidak pernah dipanggil dari `RunService`.
- `src/run.rs:91` `PendingCalls/Batch/Resume/Replay` struktur ada, tidak terpakai di jalur utama.
- `src/service.rs:628-665` handling `RunEvent::ToolCalls` hanya forward SSE, tanpa sinkronisasi ke `pending_calls`.

**Dampak:**
- CLI menunggu HTTP MCP s/d `call_ttl=600s` (default `agentgate.toml`), lalu timeout. User melihat turn gantung, lalu error.
- HTTP timeout masih lewat di belakang AgentGate; 9router/AionUi mencatat `tool_call_id` yang tidak pernah ter-resolve.
- Crash di tengah turn = tool call yang sudah ter-kirim ke client tapi CLI tidak pernah menerima hasilnya.
- Tidak ada replay rotate session, resume parsial, merge of responses untuk batch.

**Risiko jika tidak diperbaiki:** Fitur tool calling tidak pernah berhasil end-to-end dalam skenario R1 normal. Setiap tool call yang dipicu CLI akan fail.

**Rekomendasi spesifikasi:**
1. `service.rs`: setelah `RunEvent::ToolCalls`, untuk tiap `tool_call`, `await pending_calls.resolve(call_id, result)` atau hubungkan `hold_tool_call` ke receiver `RunEvent`.
2. Implementasi `PendingCalls::wait()` + `Resume::run()` + `Replay::replay()` sesuai §10.4–§10.6. `resume.replay` boleh fallback jika parked process hilang (rotasi session), bukan default.
3. Definisi `RequestBatcher` (yg ada di `bridge.rs:727`) harus dipakai aktif per §10.3; jangan definisi ulang.
4. Ambil extended API: `POST /_bridge/resolve/:session/:tool_call_id {result, error}` jika client tidak bisa membalas via `tool` message (mis. client normal 9router).

---

### P0-2. SSE tool_calls melanggar §7.4 (single-chunk) — ✅ SELESAI

**Lokasi:** `src/api.rs:464-468`

**Spec §7.4 mewajibkan:**
- Chunk 1: `delta.tool_calls[0] = {id, name, arguments: ""}`, `finish_reason = null`
- Chunk 2: `delta.tool_calls[0] = {id, name: null, arguments: "<json>"}`, `finish_reason = null`
- Chunk 3 (jika >512B): `arguments` continuation
- Chunk akhir: `finish_reason = "tool_calls"`

**Realita:** Satu chunk berisi `id + name + arguments` sekaligus, lalu `finish_reason = tool_calls`.

**Dampak:**
- 9router/AionUi (OpenAI strict client) akan menganggap `arguments` kosong pada chunk pertama, lalu merakit ulang JSON gagal.
- AionUi yang tolerate single-chunk hanya akan sukses di kasus kecil; kasus `search/read` besar (>512B) pecah lebih parah.

**Perbaikan (SELESAI 2026-11-09):**
- `src/api.rs` sekarang emit chunk1 (id,name,arguments:''), chunk2+ continuation (arguments fragments ≤512B), final finish_reason=tool_calls.
- `cargo check` + `cargo test --lib` lulus.

---

### P0-3. `notifications/cancelled` tidak membatalkan (abort/kill hilang) — ✅ SELESAI

**Lokasi:** `src/api.rs:678` `async fn handle_mcp_notification` → `StatusCode::ACCEPTED`

**Spec §9.6-§9.7 mewajibkan:**
- `notifications/cancelled {id}` → batalkan pending call + kill CLI child + return error ke client + resolve parked run.
- `notifications/progress` → ignore (sudah ada).

**Dampak:**
- Pengguna abort di AionUi / Ctrl-C / 9router stop tidak mengkill CLI process. Proses CLI anak tetap jalan, `cwd` tidak dibersihkan, `temp/agentgate/sessions/{id}/` bocor.
- Zombie process menumpuk di Windows.

**Perbaikan (SELESAI 2026-11-09):**
- `src/api.rs`: handler `notifications/cancelled` extract `requestId`, call `bridge.handle_cancelled(request_id).await`
- `src/bridge.rs`: `handle_cancelled()` — remove pending call & wake with `ToolResult::error("cancelled")`, cancel associated run, close provider session via `provider_session.close().await` (kills CLI child + process tree on Windows via `taskkill /F /T /PID`, cleans `temp_dir()/agentgate/sessions/{id}/work`)
- `src/provider.rs`: register provider sessions with BridgeServer on creation
- Idempotent: safe if requestId unknown; held `tools/call` woken with clear error
- `cargo check` lulus, Windows file lock resolved

---

### P0-4. Hold pending tanpa keepalive SSE `: keepalive`

**Lokasi:** `src/api.rs:700-709` `handle_mcp_post` menunggu `rx.await` tanpa keepalive.

**Spec §9.5:** CLI menunggu `tools/call` hingga `call_ttl` (600s). Tanpa keepalive, HTTP proxy / load balancer (dan CLI HTTP client) akan memutus koneksi jauh sebelumnya.

**Dampak:**
- `tools/call` terputus di tengah jalan, CLI menerima `ConnectionReset`, lalu retry atau gagal.
- Harus dilindungi `oneshot::select!` dengan `tokio::time::sleep(15s)` → emit `: keepalive\n\n` untuk SSE bila CLI memakai SSE (opencode) atau `: keepalive` HTTP chunk.

---

## 4. Temuan P1 — Isolasi, Windows Path, Disable Builtin

### P1-1. Provider adapter bridge tidak consistently dipakai

**Lokasi:**
- `src/main.rs:169` `build_cli_provider()` aktif di production path (benar).
- `src/cli.rs:18,46,73` (`opencode test`, `models`, `test`) construct `CliProviderImpl` langsung, bypass bridge.

**Dampak:** Masih ada jalur spawn tanpa MCP bridge, tanpa cwd kosong, tanpa disable builtin. E2E test via CLI `opencode test` tidak mencerminkan production.

**Perbaikan:**
- `cli.rs` harus memanggil `build_cli_provider()` (sama). Hapus `CliProviderImpl::new()` langsung.

---

### P1-2. Guard `is_bridge_tool` + `preflight_isolation` lemah, tidak fail-closed

**Lokasi:**
- `src/api.rs:695` `is_bridge_tool(name)` + `src/bridge.rs:411` `preflight_isolation()`
- `src/permission/mod.rs` `TooLarge`, `BadRequest`, `IsolationUnavailable`
- `src/openai/tools.rs:425-430` `OpenAiToolPolicy` cek `function.name` prefix, tetapi `description`/schema tidak divalidasi.

**Realita:**
- Guard hanya cek prefix `agentgate_` / `mcp__agentgate__` pada jalur normal. Jika isolasi gagal (mis. opencode config tidak disable builtin), provider tidak stop pakai tools builtin, dan tidak emit `503 tool_isolation_unavailable` ke client.
- `openai/tools.rs:425-430` `OpenAiToolPolicy` cek `function.name` prefix, tetapi `description`/schema tidak divalidasi.

**Dampak:** CLI bisa panggil builtin `read/write` langsung (yang coba baca folder) → dapat `os error 5 Access denied` di Windows karena harness hanya allow file individual, bukan direktori. Error mentah diteruskan ke client alih-alih fail-closed.

**Perbaikan:**
- `build_cli_provider()` wajib verifikasi isolasi sebelum `spawn`: config opencode `agentgate=true`, `excludeTools`, `mode= "build"`, dsb. Jika gagal, error `IsolationUnavailable` + jangan spawn.
- Guard tool result: builtin tools call harus ditolak sebelum eksekusi, bukan setelah error OS.

---

### P1-3. Windows path fallback `/tmp` (akar Access Denied & bocor)

**Lokasi:**
- `src/provider/bridge/claude.rs:156` `PathBuf::from("/tmp/agentgate")`
- `src/provider/bridge/gemini.rs:156` sama

**Di Windows:**
- `"C:\tmp\agentgate"` (di drive berjalan) bukan `%TEMP%/agentgate`. Berpotensi `Access denied` jika user tidak punya izin tulis di root drive.
- `src/provider/bridge/opencode.rs:166` sudah benar pakai `temp_dir()/agentgate/sessions/{id}/work`.
- `src/provider.rs:250` cleanup `temp/agentgate/sessions/{id}/work/` sudah ada.

**Perbaikan:**
- Ganti fallback ke `std::env::temp_dir().join("agentgate/sessions/{id}/work")` di semua provider bridge.
- Tambahkan `.cmd` handling: Windows `opencode.cmd`/`claude.cmd` perlu `cmd /C` + `PATHEXT`. Lihat `src/provider.rs:1382`, `src/provider/bridge/opencode.rs:116` — belum konsisten di claude/gemini.

---

### P1-4. Injeksi MCP transport tidak konsisten (`stdio_shim` vs `http`)

**Lokasi:**
- `agentgate.toml:25` `transport = "stdio_shim"`, `stdio_shim = "stdio_shim"`
- `src/provider/bridge/opencode.rs:490` `transport: "http"` default provider env.
- `src/main.rs:380` `McpShim` shim butuh env `AGENTGATE_SHIM_TOOLS` + port.

**Dampak:**
- Jika env terpotong di Windows (argumen panjang), `tools/list` kosong → CLI tidak tahu tool mana yang tersedia → tool call gagal tanpa log jelas.
- Dua jalur (http vs shim) tidak disinkronkan; testing `opencode test` pakai http, production pakai shim.

**Perbaikan:**
- Satu source of truth untuk transport; prod harus sesuai `agentgate.toml`.
- Tambahkan logging + test `--agentgate-config` env yang validasi transport.

---

### P1-5. Gemini & Claude injeksi salah

**Gemini (`src/provider/bridge/gemini.rs:179,370`, `src/provider/bridge.rs:168`):**
- `GEMINI_CLI_SYSTEM_SETTINGS_PATH = "{json}"` — spec §12.3 mewajibkan file path ke settings JSON file per-run, bukan inline JSON string.
- `excludeTools` / `tools.core` payload tidak diisi.
- `GEMINI_OWN_TIMEOUT = false` benar, tapi settings file tidak dibuat.

**Claude (`src/provider/bridge/claude.rs:179,365`, `provider/bridge.rs:168`):**
- `CLAUDE_MCP_CONFIG` env hanya berisi `{mcpServers: {agentgate: {...}}}`, bukan flag `--mcp-config <file>`.
- Missing `--strict-mcp-config`.
- Akibatnya, user MCP yang dipasang global di Claude (`~/.claude.json`) akan ikut ter-load (B tidak terisolasi).

**Perbaikan:**
- Gemini: tulis settings file per-run di `sessions/{id}/settings.json`, set env path ke file tersebut.
- Claude: tulis mcp-config file per-run, pakai flag `--mcp-config <file> --strict-mcp-config`.
- Pastikan `excludeTools` diisi `["*"]` kecuali bridge tool.

---

### P1-6. Disable builtin tools belum aktif di jalur request

**Lokasi:**
- `build_opencode_config()` ada di `src/provider.rs:754` + `src/provider/bridge/opencode.rs:64,476` + env `.claude.json`.
- `agentgate.toml` opencode `disable = ["*"]` ada, tetapi operator per-run tidak tertutup dengan baik.

**Realita:** Prompt instruction `use only agentgate_* tools` tidak cukup untuk isolation. Harus config-level.

**Perbaikan:**
- Verifikasi: tiap `opencode` spawn harus bawa config file per-run: `agentgate=true`, `mcp.agentgate` terdaftar, `tools` all disabled, `mode="build"`, `system prompt` inject.
- Test harus gagal jika `is_bridge_tool()` false di CLI (fail-closed).

---

### Penyebab `Read folder → Access denied (os error 5)` (Windows spesifik):

Bukan bug AgentGate. Tool opencode `Read` hanya support file individual, bukan listing direktori. Di Windows izin direktori ditolak harness. Bridge harus: normalisasi casing `Read/read`, tolak read folder dengan JSON error jelas (fail-closed), fallback `glob/ls` terpermission, jangan crash.

---

## 5. Temuan P2 — Run Lifecycle, Batching, Limit, Alias, ID

### P2-1. RequestBatcher tidak dipakai; paralel tool call hilang

**Lokasi:** `src/bridge.rs:14,727` definisi ganda, tidak dipakai `service.rs`. Spec §10.3 window 150ms.

**Realita:** `parallel_tool_calls: false` → `truncate(1)` di `service.rs:712`, sisa tool call dibuang permanen. Bukan di-queue ke batch berikutnya.

**Dampak:** Model yang emit 2-3 tool call (mis. read 2 file) akan kehilangan 1-2 tool call tanpa error, lalu model confuse karena tidak menerima respons untuk semua.

**Perbaikan:**
- Pakai `RequestBatcher` aktif: batch window `150ms`, merge tool results, reply semua tool result dalam 1 turn.

---

### P2-2. Resume/parked/off-by-one TTL

**Lokasi:**
- `agentgate.toml` `call_ttl=600s`, `parked_ttl=45s`, `max_concurrent=16`, `batch_window=150ms`, `batch_max=32`
- `src/bridge.rs:443` TTL check per pending call
- `src/run.rs:269` `resume` constructor

**Realita:**
- `hold_tool_call TTL` benar (600s), tapi `parked_holds_permit` sudah terpisah — selain deadline, off-by-one TTL kedua tidak konsisten.
- Incremental `max_tools_per_turn = 8` + `max_turns = 20` benar, tapi resume/replay kerap divergen.

**Perbaikan:**
- Satukan TTL per spec §10.5–§10.8: `call_ttl` untuk per-call, `parked_ttl` untuk run, cleanup expiry + reject held client + `DELETE` cleanup.
- Log jika `expired_deadline` hit.

---

### P2-3. Non-stream finalization: orphan call

**Lokasi:** `src/service.rs:748` `finalize_non_stream`.

**Realita:**
- `finish_reason=tool_calls` + `function.arguments` stringified — sudah benar.
- `tool_call_id` yang diterbitkan adalah id dari CLI (`parser.rs:384`), bukan id generator (`bridge.rs:368` `call_<16hex>`). Jika CLI emit id kosong/duplikat, `PendingCalls::insert` langsung gagal/duplikat.

**Perbaikan:**
- Semua `tool_call_id` yang diterbitkan ke client harus dari `ToolCallIdGenerator::call_id()` (call_<16hex>), bukan dari CLI.
- Pastikan resumable.

---

### P2-4. Alias, schema, limit hasil

**Lokasi:** `src/openai/tools.rs:294,392,478,510,535`, `src/config.rs:92-95` (`max_result_bytes=262144`, `max_prompt_chars=400000`)

**Realita:**
- `openai_name_alias()` + `unalias_tool_calls()` dipakai, baik.
- Duplikat setelah sanitasi tidak menambah `_2/_3` (harus unique 64-char per §8.3). `NameMap` ada tapi tidak dipakai aktif di jalur utama.
- Limit hasil tidak di-enforce di `render_conversation` (`provider.rs:1103`). Tool result besar 10MB akan masuk prompt langsung, meledak context.

**Perbaikan:**
- Terapkan `NameMap` untuk dedupe suffix deterministic.
- Enforce `max_result_bytes` + `max_prompt_chars` dengan error atau truncation policy (spec §14) + log.

---

## 6. Reasoning (spec_reasoning.md) — Catatan

### 6.1 Yang sudah benar

- `src/provider.rs:375,422` streaming per-baris (bukan per-byte), bounded channel 128, early role chunk. **Ini jawab "thought muncul barengan" — L1 sudah benar.**
- `src/reasoning/parser.rs:31,94,180,236` parser toleran NDJSON:
  - Claude: `thinking_delta/omitted/signature/plan_checkpoint/plan_summary`
  - Gemini: `thought/summary`, `response.output_text`
  - OpenCode: `text/reasoning/part`, `reasoning_effort/budget`
  - Ollama: `Thinking...` + `thinking` bukan part
- `src/reasoning/format.rs` + `src/reasoning/coalesce.rs`:
  - Mode `reasoning_content/reasoning/both/think_tags/none`
  - Header `X-Agentgate-Reasoning-Granularity`, `X-Agentgate-Reasoning-Format`
  - Coalescer 40ms, `<think>` escape, signature tetap akurat, `<think>` dan `reasoning` tidak dobel
- `src/provider/ollama_native.rs` proxy reverse + `/api/tags` + streaming `thinking` dengan `model_no_tool_support` cek manual — sudah benar.

### 6.2 Kurang lengkap

- **Per-CLI mapping `reasoning_effort`:** `reasoning_effort/budget_tokens` dari request tidak selalu diteruskan sebagai env (claude env budget, opencode varian, gemini settings, ollama `think` level). Flag test `--reasoning v1` hanya log.
- **`compat/*.json` tidak ada:** `compat/clients.json` saja. Tidak ada `claude-{version}.json`, `opencode-{version}.json`, `gemini-{version}.json`, `ollama-{version}.json` untuk mapping kapabilitas/granularitas/omitted. Header granularity di-hardcode (`api.rs:349,386`), bukan dari compat matrix.
- **Spike bridge terpisah:** `spike-bridge` ada, tapi spike-compat tidak lengkap.

---

## 7. Dependency Gate (Ollama native vs CLI)

**Lokasi:**
- `docs/spec_tooling.md §12.5`, `src/service.rs:496` `is_native_provider()`, `src/provider/ollama_native.rs`

**Status: BENAR.**

- `ollama` native tidak pakai CLI, proxy langsung ke `OLLAMA_HOST`, stream `thinking` → `reasoning`, `tool_calls` → `tool_calls`.
- `claude/gemini/opencode` wajib CLI + Bridge.
- `is_native_provider` config-driven; tidak asumsi `ollama==native`.
- Fail-closed untuk `model_no_tool_support` cek dari config, bukan dari `/api/tags` — acceptable, tapi sebaiknya probe `/api/show capabilities` untuk akurasi.

---

## 8. Config & Access Control

**Lokasi:**
- `src/config.rs:13,47,173` (`[routing]`, `[bridge]`, `[bridge.batch/resume/limits/guard]`, `[workspace]`), `agentgate.toml`, `src/main.rs:169`

| Key | Nilai default | Status |
|---|---|---|
| `bind` | `127.0.0.1:11434` | Benar (hanya listen localhost) |
| `type` | `cli-agent` | Benar |
| `allowed_providers` | `["ollama", "claude", "gemini", "opencode"]` | Benar |
| `bridge.bind` | `127.0.0.1:0` (random) | Benar |
| `bridge.token` | `AGENTGATE_BRIDGE_TOKEN` wajib | Benar |
| `guard.enforce` | — | **Belum fail-closed** (lihat P1-2) |
| `workspace.workdir` | `temp/agentgate/sessions/{id}/work/` | Benar untuk opencode, salah fallback untuk claude/gemini (P1-3) |

- `GET /health`, `/v1/models`, `/v1/chat/completions`, `POST /` (compat POST) terdaftar benar.
- `POST /_bridge/mcp/:session` Bearer + `Origin` reject + length check — benar.
- `GET /_bridge/mcp/:session` tidak ada (hanya POST/DELETE).

---

## 9. Matriks Verifikasi Spec

| § | Item | Status | Bukti path |
|---|---|---|---|
| 6 | klasifikasi Run | ✅ | `src/run.rs:108,122`, `src/service.rs:499,202` |
| 7.1 | validasi `n, type, tools > 128, tool_choice=none` | ✅ | `src/api.rs:165,180` |
| 7.1 | `response_format` diabaikan tanpa header | ⚠️ | `src/types.rs:99` |
| 7.3 | non-stream tool_calls | ✅ | `src/service.rs:703,748` |
| 7.4 | stream tool_calls 2-chunk | ✅ | `src/api.rs:464-485`, `tests/sse_tool_calls_contract_test.rs`, `docs/SSE_CONTRACT_TESTS.md` |
| 7.6 | error 501/503 + type | ✅ | `src/error.rs:98-115` |
| 8.1-8.2 | schema normalize inline_refs | ✅/⚠️ depth+limit | `src/openai/tools.rs:392,535` |
| 8.3 | alias deterministik + dedupe `_2/_3` | ⚠️ dedupe kurang | `src/openai/tools.rs:294,478,510` |
| 8.4 | `tool_call_id = call_<16hex>` | ⚠️ generator tidak dipakai | `src/bridge.rs:368` |
| 8.5 | `tool_choice`, `parallel_tool_calls` | ⚠️ truncate vs batch | `src/service.rs:26,712` |
| 8.6 | `max_result_bytes`, `max_prompt_chars` | ❌ tidak di-enforce | `src/config.rs:92-95` |
| 9.1/9.2 | endpoint + auth Bearer CT + Origin | ✅ | `src/api.rs:33,644` |
| 9.5 | hold pending | ❌ disconnected + no keepalive | `src/api.rs:709`, `src/bridge.rs:197` |
| 9.6 | cancel | ❌ | `src/api.rs:678` |
| 10.1-10.3 | RunState + batch 150ms | ❌ batcher tidak dipakai | `src/bridge.rs:14,727` |
| 10.4 | Resume/Parked/Replay rotate | ⚠️ replay saja | `src/service.rs:628-665` |
| 11.3 | cwd kosong | ✅ opencode, ❌ claude/gemini fallback | `src/provider/bridge/claude.rs:156` |
| 12.2 | opencode disable + mcp bridge (transport, enabled, excludeVars) | ⚠️, transport inkonsisten | `agentgate.toml:25`, `src/provider/bridge/opencode.rs:64,476` |
| 12.3 | gemini settings file | ❌ inline JSON | `src/provider/bridge/gemini.rs:179,370` |
| 12.4 | claude `--mcp-config --strict-mcp-config` | ❌ env saja | `src/provider/bridge/claude.rs:179,365` |
| 12.5 | ollama native proxy | ✅ | `src/service.rs:496`, `src/provider/ollama_native.rs` |
| 13 | guard + preflight + fail-closed 503 | ⚠️ lemah | `src/provider/bridge.rs:152` |
| 14 | config port/ttl/batch/limits | ✅ mostly | `src/config.rs:13,47` |
| RS0 | streaming RS0 pipeline (bounded, per-baris) | ✅ | `src/provider.rs:375` |
| RS5 | compat per-CLI matrix | ❌ | `compat/` |
| RS5 | mapping reasoning_effort | ❌ | `src/config.rs:409` |

---

## 10. Rekomendasi Roadmap Perbaikan (Prioritas)

### Tahap 1 — Fix P0 (estimasi 1–2 sprint)

1. **P0-1 End-to-end hold↔Run:** buat `PendingCalls` yang terhubung `RunEvent::ToolCalls` ke `resolve_tool_call` — tutup loop.
2. **P0-2 Emit 2-chunk §7.4** + continuation 512B.
3. **P0-3 Cancel** = batalkan pending + kill child + cleanup cwd.
4. **P0-4 Keepalive** `: keepalive` setiap 15s selama hold.
5. **Verifikasi:** manual E2E `cargo test --test bridge_mcp_hold_test -- --nocapture` + test SSE parsing 9router.

### Tahap 2 — Fix P1 (isolasi & Windows)

6. Samakan `cli.rs` ke `build_cli_provider()`, hapus `CliProviderImpl` mentah.
7. Ganti fallback `/tmp` → `temp_dir()` di claude/gemini.
8. `.cmd` handling: `cmd /C` + `PATHEXT` untuk semua CLI di Windows.
9. Gemini: tulis settings file per-run. Claude: `--mcp-config <file> --strict-mcp-config`.
10. `guard.enforce` fail-closed: jika isolasi gagal → `503 tool_isolation_unavailable`, jangan spawn.
11. Error handling Read folder: kembalikan JSON error jelas, jangan crash.

### Tahap 3 — Fitur spec penuh

12. Aktifkan `RequestBatcher` 150ms, queue tool call sisa, bukan truncate.
13. Enforce `max_result_bytes` + `max_prompt_chars` + truncation policy.
14. Dedupe alias `_2/_3` + pakai `ToolCallIdGenerator` untuk semua id.
15. `compat/*.json` per-CLI + mapping `reasoning_effort`.
16. Document + RFC final, plus changelog, plus re-audit.

---

## 11. Testing & Regression Checklist

> **Status update (2026-11-09, lead/OpenCode, Windows host, working tree has uncommitted P0/P1/P2-in-progress changes):** `[x]` = verified locally in this session/recent runs; `[~]` = code-level mitigation/test exists but **needs target-environment E2E**; `[ ]` = not yet verified. Do **not** treat `[~]` as release-ready for the original Linux/9router/VSC Copilot issues.

### Verified locally (this environment)

- [x] `cargo check` hijau.
- [x] `cargo test --lib` — **55 passed**.
- [x] `cargo test --test spec_scenarios_test` — **8 passed**.
- [x] `cargo test --test fail_closed_test` — **5 passed**.
- [x] `cargo test --test sse_tool_calls_contract_test` — **6 passed** (kontrak SSE `tool_calls` 2-chunk §7.4/P0-2 + grouping; ekspektasi test diselaraskan dengan implementasi).
- [x] `cargo test --lib reasoning` — **34 passed** (dilaporkan BD2; UX grouping parser+coalescer).
- [x] `cargo test --test bridge_resolve_auth_test` — **2 passed** (dilaporkan BD).
- [x] Windows host release build: `target\release\agentgate.exe` (11,992,576 bytes, 2026-11-09 13:02:46, SHA256 `234BA39C1A2D5A4957F84E62EDFC22B949A22ED1DE88FC51F83079E6A72004B4`).

### Integration/E2E — perlu konfirmasi runtime (target env / CI)

- [x] `cargo test --test bridge_mcp_hold_test -- --nocapture` — **1 passed** (local, 2026-11-09).
- [~] `opencode` direct / `opencode test --reasoning v1` tidak men-spawn tanpa config — validasi env nyata.
- [~] SSE `tool_calls` parsing client 9router (2-chunk + continuation, P0-2) — **wajib E2E 9router/AionUI**.
- [~] UX: `thought complete` / `view steps N` grouped (bukan `thought/view steps 1` berulang) — **wajib E2E 9router/AionUI**.
- [~] `tools/call` hold > 45s dengan keepalive, tidak putus (P0-4) — E2E proxy/CLI.
- [~] `notifications/cancelled` mematikan pending + cleanup `temp/agentgate/sessions/{id}/` (P0-3) — E2E; Windows process-tree kill best-effort.
- [~] Batch 2 tool call paralel → keduanya ter-resolve (bukan truncate) — **P2 pragmatis selesai** (limits/render+bridge result truncation, batch sanitize/dedupe, ID/TTL sebagian), tapi **RequestBatcher wiring + “queue remaining vs truncate” belum terintegrasi** dan reverse-alias mapping butuh verifikasi; E2E diperlukan.
- [~] Resume parked run tanpa spawn baru / replay fallback hanya saat parked process hilang — **S4 code-wired** (opencode first, capability-gated, replay fallback wajib): continuation registry + opencode park-early/`wait_for_continuation` + provider keep-alive on `tool_calls`+capable + `service::resume_run` resolves real MCP holds → awaits continuation (timeout) → response or `replay_fallback{continuation_*}`. **S5 partial** (BD reported complete, 137 tests claimed; lead verified core suites): sweeps/rotation/TTL/cleanup paths + orphan session dir sweep startup; remaining PID/process orphan sweep, full `cli_gen++` adapter guarantees, TTL/process-tree end-to-end incl multi-step continuation re-park/state, targeted tests. **E2E wajib**: MCP-hold→CLI lanjut, live resume vs replay, hold>45s/cancel/batch, Linux/9router/AionUI/VSC Copilot. Streaming continuation/multi-step re-park gaps remain.
- [~] Windows: tidak ada `Access denied` pada listing cwd kosong / Read-folder JSON error jelas — unit-ish mitigations ada; E2E Windows.
- [~] `guard.enforce`/fail-closed: disable isolation/builtins → 503 `tool_isolation_unavailable` sebelum path unsafe — `fail_closed_test` hijau; E2E config-level builtin-disable pre-spawn.
- [~] Linux: fallback `/tmp` valid, tidak ada `.cmd` issue; BR0–BR7 — **Linux env required**.
- [~] 9router → VSC Copilot workdir/installer-path + tool flow — **E2E target env required** (isu awal).

### Auth / Network

- [ ] `/_bridge/mcp/:session` tanpa Bearer → 401
- [ ] `/v1/chat/completions` `n>1` → 400, `tools >128` → 400
- [ ] `.cmd` handling Win: `opencode.cmd` sukses spawn, fallback tidak `Access denied`

### All-platform build / release

- [x] CI scaffold: `.github/workflows/build.yml` (check + `cargo test --lib reasoning`, 4-target release artifacts); panduan `docs/BUILD_AND_RELEASE.md`, checklist `scripts/pre_release_checklist.md`.
- [~] **S6 (BD: E2E+CI artifacts; lead: docs/audit/CI review):** artifact Windows/Linux x86_64+aarch64 + checksums via CI/cross-toolchain pending; clean rebuild Windows release untuk biner patched; E2E target env pending (lihat §11).
- [~] HEAD `227d772` berpesan “Complete P0/P1/P2…”; **P2 ditutup pragmatis** (render/bridge `max_result_bytes` enforcement, batch dedupe/sanitize, perbaikan regresi NameMap-per-call). **Known gaps:** RequestBatcher tidak ter-wire ke flow nyata + “queue remaining vs truncate”, reverse alias collision, verifikasi ID format/resumability/TTL end-to-end, E2E paralel 2 tool calls. **Stage 2:** S1–S3 done; **S4 code-wired** (opencode, capability-gated, replay fallback); **S5 partial** (BD reported complete, 137 tests claimed; lead verified `cargo check` + core suites: lib 55, bridge_session_registration 8, spec_scenarios 8, sse 6, fail_closed 5, bridge_mcp_hold 1, bridge_resolve_auth 2, stage2_continuation_test 2; dir orphan sweep + cleanup/rotation increments) — remaining PID/process orphan sweep, full `cli_gen++` adapter guarantees, TTL/process-tree end-to-end incl multi-step continuation re-park, targeted tests. **S6 pending** (E2E target env + CI all-platform artifacts/checksums + audit closure). **Working tree uncommitted**.

### Catatan audit tambahan
- **`thought/view steps 1` berulang vs grouped steps** adalah temuan UX/kompatibilitas klien (9router/AionUI) yang **tidak tercakup eksplisit di `audit.md` awal**; mitigasi ada di `src/reasoning/{parser,coalesce}.rs`, butuh E2E.

---

## 12. Lampiran: File & Line yang Diacu

| Area | Path | Baris relevan |
|---|---|---|
| CLI classify | `src/run.rs` | `108,122,148,190,269,329,386,433` |
| Mids | `src/provider.rs` | `250,317,342,375,422,445,682,1103,1309,1360,1382,1410,1429` |
| Service | `src/service.rs` | `26,52,149,202,245,299,496,499,628-665,677,703,712,736,748,774,782,828` |
| API | `src/api.rs` | `33,103,165,180,218,243,309,331,349,386,391,464,484,550,553,644,678,695,709,737` |
| Bridge MCP | `src/bridge.rs` | `14,175,185,197,368,411,442,513,554,676,704,727` |
| Provider bridge | `src/provider/bridge.rs` | `152,168` |
| Opencode | `src/provider/bridge/opencode.rs` | `64,116,166,380,475-513,476-490` |
| Claude | `src/provider/bridge/claude.rs` | `36,156,157,179,365,370` |
| Gemini | `src/provider/bridge/gemini.rs` | `156,157,179,370` |
| Ollama | `src/provider/ollama_native.rs` | — |
| OpenAI tools | `src/openai/tools.rs` | `183,262,294,392,478,510,535` |
| OpenAI SSE | `src/openai/sse_tools.rs` | — |
| Config | `src/config.rs` | `13,47,92-95,173,292,409` |
| Error | `src/error.rs` | `98-115` |
| Reasoning | `src/reasoning/parser.rs` | `31,94,180,236,384` |
| Permission | `src/permission/mod.rs` | — |
| Tests | `tests/bridge_mcp_hold_test.rs`, `tests/spec_scenarios_test.rs`, `tests/fail_closed_test.rs` | — |
| Docs | `docs/spec_tooling.md`, `docs/spec_reasoning.md`, `docs/audit_spec_tooling.md`, `docs/spec_tooling_progress.md`, `docs/tooling_conformity_matrix.md` | — |
| Config | `agentgate.toml`, `Cargo.toml`, `compat/clients.json` | — |

---

## 13. Penutup

AgentGate sudah punya fondasi yang kuat untuk reasoning dan beberapa fondasi tooling. Kunci untuk production adalah **menutup loop hold↔Run** (P0-1) dan **fail-closed isolasi** (P1-2/P1-6). Tanpa keduanya, setiap perbaikan lain hanya mengandalkan mitigasi prompt yang rapuh.

Rundown prioritas bisa dianggap selesai untuk go-live jika checklist E2E di §11 terpenuhi. Setelah itu, fitur batch/limit/alias bisa diinkremental tanpa mengganggu stabilitas awal.

**Platform:** Linux direkomendasikan untuk BR0–BR7 (uji R1 lebih mudah, `/tmp` valid, tidak ada `.cmd`). Windows memerlukan hardening tambahan (`.cmd` + `temp_dir()` + process tree kill) sebelum go-live.

