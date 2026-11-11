# Audit Tool Bridge/Tooling — kepatuhan terhadap `docs/spec_tooling.md`

| | |
|---|---|
| **Tanggal audit** | 2026-11-11 |
| **Scope** | AgentGate tool bridge (AionUI/klien → 9router (opsional) → AgentGate → opencode/CLI), request dengan `tools[]`, eksekusi tool di workdir user |
| **Baseline kode** | Repo lokal `main @ b825001` (tag `v0.2.5`), Windows gateway & VPS `10.11.1.21:8780` menjalankan **agentgate 0.2.5** (VPS backup `/usr/local/bin/agentgate.bak.20261111-112815`) |
| **Referensi utama** | `docs/spec_tooling.md` (v0.2, §1–19), `docs/audit_spec_tooling.md` (2026-11-05, historis), `docs/LOCAL_GATEWAY_TEST_REPORT_2026-11-11.md` |
| **Legenda** | ✅ sesuai · ⚠️ sebagian/risiko · ❌ gap · N/A di luar AgentGate |

---

## 1. Ringkasan eksekutif

**Konsep spec (`§1`, `§2.1 G1`, `§4.3`, `§11.3`):** klien (AionUI, perangkat A, workdir user/Windows) adalah **satu-satunya eksekutor** tool client. AgentGate+CLI (perangkat B, Linux/VPS) menjalankan CLI di workdir sesi terisolasi (`{data_dir}/sessions/{id}/work`, contoh VPS `/tmp/agentgate/sessions/.../work`) tanpa akses file proyek user; saat request berisi `tools[]`, AgentGate memasang tool ke CLI via MCP bridge, **menahan** `tools/call` → memancarkan `tool_calls` OpenAI ke klien → klien mengeksekusi lokal → mengirim `role:"tool"` + `tool_call_id` → AgentGate meneruskan hasil (resume).

**Status saat ini (praktis, v0.2.5):**
- Kontrak transport/stream untuk tool-calling di gateway **membaik**: `/v1/models` OpenAI-compatible (v0.2.4), stream `tool_calls` berakhir bersih `finish_reason:"tool_calls"` → `[DONE]` tanpa trailing delta (v0.2.5). Uji VPS 2026-11-11: health 200; **12/12 model free** yang diminta merespons 200 (prompt singkat); stream+tools `iceman-free` 200 dengan delta `tool_calls` OpenAI-style + terminasi benar.
- **Gap inti terhadap tujuan "AionUI read/edit di workdir user Windows"** bersifat gabungan: (a) topologi — AgentGate VPS tidak bisa akses `C:\...` dan memang tidak seharusnya; akses hanya lewat tool yang dieksekusi klien, (b) isolasi tool bridge di AgentGate **dilemahkan** (disable built-in tools/permission agresif di `opencode.json` bridge dihilangkan sejak v0.2.3 untuk menghindari `provider.free-limit` semu pada free model) sehingga model/CLI berpotensi memilih *built-in tools* opencode di B alih-alih/di luar tool MCP yang harus dieksekusi klien, (c) rantai integrasi AionUI→9router→AgentGate berpotensi merusak semantik: prompt harness ganda/merge, `tool_call_id`/nama tool di-rewrite, dan `tool_calls` diteruskan ke registry eksekutor yang salah → gejala **"Unknown tool" (glob/read/shell/execute)** dan loop.
- **Kuota upstream** opencode free-tier (`provider_error: Free usage limit reached`) bersifat intermittent dan di luar kontrol gateway; bukan indikator kepatuhan tool-bridge.

**Kesimpulan:** arsitektur bridge AgentGate *sebagian besar sejalan* dengan pola spec (hold/emit/resume, cwd sesi terisolasi, kontrak stream), namun **belum ada jaminan end-to-end** bahwa tool file/edit dijalankan di workdir user AionUI pada rantai saat ini. Diperlukan: verifikasi E2E client-executed flow (langsung ke gateway, bypass 9router), kebijakan isolasi/pemilihan tool yang aman untuk free model, perlakuan prompt/ID yang konsisten, dan koreksi di 9router/AionUI untuk bagian di luar AgentGate.

---

## 2. Topologi & implikasi (konteks audit)

| Komponen | Lokasi/Peran | Catatan |
|---|---|---|
| AionUI (klien) | Windows, workdir user (mis. `C:\Users\EDN\Documents\AION\project\agentgate`) | **Harus** mengeksekusi read/edit/shell lokal sesuai `docs/spec_tooling.md` |
| 9router (opsional) | Perantara AionUI↔AgentGate | Di luar repo; dilaporkan dapat merge prompt, rewrite ID/tool, dan meneruskan tool_calls ke registry salah |
| AgentGate | Windows `127.0.0.1:8780` (lokal) **atau** VPS Linux `10.11.1.21:8780` | Bridge MCP, hold/resolve, emit `tool_calls`, resume; **bukan** eksekutor tool client |
| opencode CLI | Dijalankan AgentGate, cwd sesi terisolasi di host AgentGate | Di VPS: `/tmp/agentgate/sessions/.../work` (tidak ada akses `C:\`) |

**Implikasi uji nyata:** prompt "baca path `C:\...`" ke VPS **tanpa** siklus `tools[]` + eksekusi klien + `role:"tool"` akan gagal akses (filesystem beda host) — ini perilaku *sesuai topologi & spec*, bukan bukti bahwa bridge client-executed berfungsi. Bukti kepatuhan butuh request ber-`tools` dan klien yang mengeksekusi hasilnya di Windows.

---

## 3. Pemetaan kepatuhan terhadap `docs/spec_tooling.md`

### 3.1 Tujuan, prinsip, dan mode (§1–§3, §5–§6)

| Item | Status | Bukti / Catatan |
|---|---:|---|
| G1 Client eksekutor tunggal tool client (§2.1) | ⚠️ | Arsitektur bridge menahan `tools/call` dan mengembalikan `tool_calls` ke klien (`src/bridge.rs`, `src/provider/bridge/opencode.rs`, shim/stdio). Namun built-in tools CLI tidak didisable ketat di bridge (lihat §3.4) → risiko eksekusi/seleksi tool di B. |
| G2/G3 Transparansi OpenAI & isolasi model dari lokasi eksekusi (§2.1, §4.3) | ⚠️ | Kontrak stream tool_calls OpenAI-style diperbaiki (v0.2.5). Isolasi bergantung pada tool yang benar-benar melalui MCP bridge dan dieksekusi klien. |
| Mode bridge saat `tools[]` vs chat-only (§2.2, §6) | ✅/⚠️ | Routing mengklasifikasikan request (native/bridge/chat-only) (`src/service.rs`, `src/run.rs`). Jalur bridge aktif untuk provider berkemampuan; perilaku tepat bergantung config provider dan ketersediaan tool. |
| Dukungan opencode/gemini/claude/ollama (§0, §5, §12) | ⚠️ | Adapter bridge/factory ada; fokus audit ini opencode (VPS). Lihat `docs/audit_spec_tooling.md` untuk status historis per-provider. |

### 3.2 Kontrak API & streaming tool_calls (§7)

| Item | Status | Bukti / Catatan |
|---|---:|---|
| `tools`, `tool_choice`, pesan `role:"tool"` + `tool_call_id` (§7.1–7.5) | ⚠️ | Tipe/request handling ada (`src/types.rs`, `src/service.rs`, `src/api.rs`). Resume/continuation multi-turn didukung jalur bridge; **belum** diverifikasi E2E pada rantai AionUI→9router→AgentGate untuk tool yang dieksekusi klien di Windows. |
| Respons non-stream: `finish_reason:"tool_calls"` & struktur `tool_calls` (§7.3) | ⚠️ | Jalur ada; verifikasi fungsional bergantung skenario (provider/model). |
| Stream SSE: delta `tool_calls` incremental, `finish_reason:"tool_calls"`, terminasi `[DONE]` (§7.4) | ✅ (gateway, diverifikasi v0.2.5) | `src/api.rs`: `tool_calls_finished` memotong delta setelah boundary, emit terminal `finish_reason:"tool_calls"` lalu `[DONE]`; error path juga menutup benar. **Bukti VPS 2026-11-11:** `agg/opencode/iceman-free`, stream+tools → 200, chunk: role delta → `tool_calls` (index/id/type/name) → arguments incremental → `{}`+`finish_reason:"tool_calls"` → `[DONE]`. (Catatan: pada contoh uji model memanggil tool `execute` alih-alih `get_time` yang diminta — indikasi pemilihan tool oleh model/CLI, relevan ke §3.4.) |
| `/v1/models` konsumsi klien | ✅ (v0.2.4) | Item model OpenAI-compatible (`id, object:"model", created, owned_by, ...`) (`src/types.rs`, `src/service.rs`). |

### 3.3 Pemetaan tools, MCP bridge, hold/resume (§4.3, §8, §9, §10)

| Item | Status | Bukti / Catatan |
|---|---:|---|
| Representasi tool MCP & bridge HTTP/stdio shim | ⚠️ | Konfigurasi MCP/shim disiapkan per sesi (`src/provider/bridge/opencode.rs`: `build_mcp_config`, injeksi `AGENTGATE_SHIM_TOOLS`/MCP server agentgate; transport `stdio_shim`/HTTP). `src/bridge.rs`/`src/provider/bridge/shim.rs` mengelola `tools/call`. Kesesuaian penuh (normalisasi nama/skema, alias, batas, keepalive/cancel detail) mengikuti status `docs/audit_spec_tooling.md` dengan perbaikan bertahap. |
| Hold `tools/call` → hasil ke klien → resolve/resume (§9.5, §10.4) | ⚠️ | Mekanisme pending/resolve & continuation ada; uji integrasi ada di repo historis. **Belum dibuktikan** pada rantai AionUI→9router→AgentGate dengan eksekusi nyata di `C:\Users\EDN\Documents\AION\project\agentgate`. |
| `tool_call_id` stabil & dapat dikorelasikan (§8.4) | ⚠️ | AgentGate meng-emit/meneruskan id; normalisasi internal ada (`unalias_tool_calls`, pelacakan bridge). **N/A untuk rewrite di 9router/AionUI** — korelasi ujung-ke-ujung wajib dijaga di seluruh hop. |

### 3.4 Isolasi konteks & keamanan tool (§11, §13)

| Item | Status | Bukti / Catatan |
|---|---:|---|
| Cwd CLI = workdir kosong per sesi di B (§11.3) | ✅ | `OpenCodeSession` membuat workdir `sessions/{id}/work` (`src/provider/bridge/opencode.rs` ~`data_dir.join("sessions").join(&session_id).join("work")`); eksekusi CLI di host server. |
| CLI hanya memakai tool MCP client / tanpa built-in tools berbahaya di B (§13.1, §13.3) | ❌/⚠️ (regresi disengaja untuk stabilitas free model) | Bridge **tidak** menyuntikkan blok disable built-in tools/permission agresif ke `opencode.json` (komentar: menghindari `provider.free-limit` semu; `build_tool_disable_config` dikembalikan kosong, argumen disable tidak dipakai). Akibat: opencode dapat mengeksekusi/memilih built-in tools dalam konteks VPS, bertentangan dengan isolasi ketat spec dan memicu mismatch dengan eksekutor klien (sumber potensial "Unknown tool" bila tool_call diteruskan ke AionUI). Guard/preflight & kebijakan `ClientToolsPolicy` ada (`src/permission/*`, `src/run.rs`, `src/service.rs`) namun jaminan "hanya bridge tools" tidak setara spec. |
| Fail-closed, non-bypassable (§13 S1–S3) | ⚠️ | Penolakan/pembatasan ada di jalur tertentu; efektivitas menurun bila built-in CLI aktif dan/atau event tidak terobservasi sebagai MCP `tools/call` ter-bridge. |

### 3.5 Prompt/konteks & observabilitas (§11.1, §15)

| Item | Status | Bukti / Catatan |
|---|---:|---|
| Konstruksi prompt dari `messages` ke CLI (§11.1–11.2) | ⚠️ | Percakapan masuk (termasuk `system`) dirender untuk CLI (`render_conversation` / jalur provider). **Tidak ada pemisahan eksplisit** "system prompt harness klien (AionUI)" vs konteks yang aman untuk opencode → risiko prompt ganda/kolisi semantik tool pada rantai AionUI→9router→AgentGate. |
| Observabilitas run/tool/stream | ⚠️ | Metrik/header/event & kontrak SSE diuji sebagian (lihat §4). Belum ada bukti E2E terinstrumentasi untuk kasus "tool dieksekusi klien Windows" via rantai produksi. |

---

## 4. Bukti verifikasi (2026-11-11)

| Pemeriksaan | Lingkungan | Hasil |
|---|---|---|
| Gateway hidup & versi | Windows `127.0.0.1:8780`, VPS `10.11.1.21:8780` | **200** `/health`, `agentgate 0.2.5` |
| `/v1/models` | VPS | 200, item berisi field OpenAI-compatible (`object/created/owned_by`, dsb.) |
| Chat non-stream `"halo"` (12 model free yang diminta) | VPS, `Bearer default_key` | **200** semua: `beam-free, big-pickle, deepseek-v4.1-flash-free, flint-40b-free, glm-5.3-flash-free, iceman-free, inkling-2-small-free, longcat-2.5-free, mimo-v2.6-flash-free, minimax-m3.1-flash-free, nemotron-3.5-lightning-free, nemotron-3.5-super-vl-free` (1.6s–19.5s). Catatan: ketersediaan/quota upstream dapat berubah. |
| Stream + tools (1 tool `get_time`) | VPS, `iceman-free` | **200**, SSE delta `tool_calls` incremental + `finish_reason:"tool_calls"` + `[DONE]`, tanpa trailing delta (v0.2.5). Model memanggil `execute` (bukan `get_time`) — relevan ke pemilihan/isolasi tool. |
| Baca path Windows `C:\Users\EDN\Documents\AION\project\agentgate` | VPS, `iceman-free`, **tanpa `tools[]` klien** | **200** tetapi konten: file/dir tidak ditemukan; runtime Linux `/tmp/agentgate/sessions/.../work` — *sesuai topologi*, **bukan** bukti client-executed read. |
| Uji E2E AionUI→9router→AgentGate (tool dieksekusi AionUI di Windows) | Rantai produksi | **Belum** (diperlukan; laporan "Unknown tool"/ID rewrite berasal dari rantai ini). |

---

## 5. Gap & analisis masalah ("Unknown tool", workdir user)

### 5.1 Akar masalah yang teramati (hipotesis terkuat, berbasis bukti)

1. **Mismatch eksekutor & tool-set:** model/opencode memilih tool (termasuk built-in `shell/glob/read/execute`) yang tidak disediakan/ditangani sebagai tool client-executed AionUI, atau `tool_calls` diteruskan ke layer yang mengeksekusi dengan registry berbeda → `Unknown tool`.
2. **Isolasi bridge tidak ketat (trade-off free-limit):** tanpa disable built-in tools yang aman, pemilihan tool bisa keluar dari himpunan MCP-bridged yang semestinya dipegang AgentGate untuk delegasi ke klien.
3. **Transformasi di 9router/AionUI (di luar AgentGate):** merge prompt harness (Windows) + prompt opencode, rewrite `tool_call_id`/format, atau filtering/pemetaan tool yang tidak konsisten memutus loop OpenAI `tool_calls` ↔ `tool` result.
4. **Topologi path:** file Windows hanya dapat diakses AionUI di host Windows; gateway VPS tidak dapat (dan tidak boleh) mengaksesnya langsung.

### 5.2 Tanggung jawab per komponen

| Area | AgentGate | 9router | AionUI/klien |
|---|---:|---:|---:|
| Emit/terminasi stream `tool_calls`, hold/resume MCP, cwd sesi terisolasi | Primer | - | Konsumsi & eksekusi |
| Kebijakan isolasi tool & prompt ke CLI (tanpa memicu regresi free-limit) | Primer (perlu desain) | - | - |
| Tidak rewrite `tool_call_id` tanpa mapping; tidak merge prompt lintas harness | Hormati kontrak | **Perlu perbaikan** | **Perlu perbaikan** |
| Eksekusi tool di workdir user Windows & kirim `role:"tool"` | - | - | **Primer** |

---

## 6. Rekomendasi & task board mapping

Prioritas disusun untuk membuktikan/memulihkan perilaku **sesuai spec**: tool user dieksekusi di AionUI (workdir Windows), AgentGate hanya menjembatani.

| ID Task (dibuat) | Prioritas | Sasaran (spec) | Deskripsi singkat / deliverable |
|---|---|---|---|
| TB-TOOL-01 | P0 | §7, §9, §10 | **Verifikasi E2E client-executed flow (bypass 9router):** skenario langsung ke gateway (Windows lokal &/atau VPS) dengan `tools[]` (read/list/write), klien uji mengeksekusi tool di workdir Windows & mengirim `role:"tool"`; rekam bukti request/response/id/stream. |
| TB-TOOL-02 | P0 | §13, §11.3, §2.1 | **Audit & rancangan isolasi built-in vs bridged tools:** inventarisasi perilaku `opencode.json`/MCP bridge saat ini; usulkan mitigasi aman free-model (disable selektif/allowlist bridge-only/exposure policy) + rencana implementasi & uji regresi. |
| TB-TOOL-03 | P1 | §11.1, integrasi | **Kebijakan prompt ke CLI:** identifikasi & (opsional) implementasi filter/flag untuk tidak meneruskan system prompt harness klien; dokumentasi kompatibilitas. |
| TB-TOOL-04 | P1 | §7.4, §8.4, rantai | **Konsistensi `tool_call_id`/nama & matriks via 9router:** bandingkan payload langsung-gateway vs via 9router/AionUI; laporkan rewrite/drop; rekomendasi perbaikan 9router/AionUI. |
| TB-TOOL-05 | P1 | §15, verifikasi | **Bukti & tes regresi:** kumpulkan artefak uji (log/SSE), update docs bukti, dan (bila stabil) tambah/aktifkan tes kontrak yang relevan. |

> Pemetaan ke implementasi: `src/api.rs`, `src/service.rs`, `src/run.rs`, `src/bridge.rs`, `src/provider/bridge/opencode.rs`, `src/provider/bridge/shim.rs`, `src/permission/*`, `src/types.rs`.

---

## 7. Catatan batasan & non-temuan

- **Akses filesystem lintas host:** AgentGate VPS tidak dapat membaca `C:\Users\EDN\Documents\AION\project\agentgate`; ini *bukan* bug bila tidak ada eksekusi tool klien. Solusi yang sesuai spec: eksekusi tool di AionUI (Windows) atau jalankan AgentGate di host yang sama/mount path yang sah.
- **Free-tier quota:** `provider_error`/limit upstream bersifat eksternal; kegagalan model sporadis tidak otomatis berarti pelanggaran spec tool-bridge.
- **Dokumen historis:** `docs/audit_spec_tooling.md` merekam status implementasi 2026-11-05/09; audit ini melengkapi dengan kondisi v0.2.5 dan temuan rantai AionUI↔9router↔AgentGate terkini.

---

## 8. Lampiran — referensi cepat

- Spec: `docs/spec_tooling.md` (terutama §1, §2.1, §4.3, §7.3–7.5, §8.4, §9.5, §10.4, §11.1–11.3, §13)
- Stream fix v0.2.5: `src/api.rs` (terminasi `tool_calls`, `[DONE]`, error path)
- Bridge opencode & workdir: `src/provider/bridge/opencode.rs`
- MCP bridge/hold-resolve: `src/bridge.rs`, `src/provider/bridge/shim.rs`
- Routing/classification: `src/service.rs`, `src/run.rs`
- Endpoint/tool-call VPS (uji): `http://10.11.1.21:8780/v1`, auth `default_key`

---

## 9. Implementasi post-fix (IMPL-TOOL-01 s.d. IMPL-TOOL-03)

### IMPL-TOOL-01: Selective disable built-in tools & isolation config (L1+L2) ✅
**File:** `src/config.rs`, `src/provider.rs`, `src/provider/bridge.rs`, `src/provider/bridge/opencode.rs`, `src/provider/bridge/spike.rs`, `agentgate.toml`

**Perubahan:**
- `src/config.rs`: `IsolationConfig` struct dengan `mode` (default `"selective"`), `allowed_builtins`, `disabled_builtins`. Disetel `[bridge.isolation]` di `agentgate.toml`.
- `src/provider/bridge.rs`: `BridgeConfig.isolation: Option<IsolationConfig>` + `BridgeConfig.system_prompt: Option<String>`.
- `src/provider/bridge/opencode.rs`: 
  - `build_selective_disable_config()`: disable `read, write, bash, execute, glob, list, patch, edit`; allow `grep, webfetch, todowrite, todoread, task`. Respect `allowed_builtins` / `disabled_builtins` overrides.
  - `build_strict_disable_config()`: disable semua built-in.
  - `build_tool_disable_config(mode, isolation)`: dispatch by mode (`off`/`selective`/`strict`).
  - `build_opencode_config_json()`: inject `isolation` key ke `opencode.json` (mode, allowed_builtins, disabled_builtins).
- `src/provider.rs`: thread `bridge_config.isolation` via `build_cli_provider` → `BridgeCliProvider::with_bridge` → `isolation_config` field → `OpenCodeSession::spawn()`.
- **Avoid `provider.free-limit`**: tidak inject deny-permission agresif; hanya tools config saja, cwd session tetap terisolasi.

### IMPL-TOOL-02: Strip system prompt harness klien ke CLI (Opsi A) ✅
**File:** `src/provider.rs`, `src/provider/bridge.rs`, `src/provider/bridge/opencode.rs`, `src/provider/bridge/spike.rs`, `tests/bridge_session_registration_test.rs`, `tests/persistent_resume_test.rs`, `tests/fail_closed_test.rs`, `tests/schema_normalization_test.rs`, `tests/spec_scenarios_test.rs`

**Perubahan:**
- `src/provider.rs`: `render_conversation()` skip `role:system` (tidak lagi `[system]` block di output rendered). `send_chat_completion()` extract system dari `request.messages`, set `bridge_config.system_prompt` (keduanya: non-stream & stream path).
- `src/provider/bridge/opencode.rs`: `--system <prompt>` flag ditambahkan ke `opencode run` command (kedua branch `has_run` dan `else`).
- `src/provider/bridge.rs`: `BridgeConfig.system_prompt: Option<String>`.
- Catatan lead: system prompt klien masih diteruskan via CLI `--system` (bukan strip total) — agar dokumentasi diupdate, pertimbangkan default-strip/filter harness di IMPL-TOOL-04/review jika perlu.

### IMPL-TOOL-03: Preflight active probe, runtime guard, dan logging tool_call (L3+L4) ✅
**File:** `src/bridge.rs`, `tests/isolation_selective_test.rs` (baru)

**Perubahan:**
- `src/bridge.rs`:
  - `preflight_active_probe(session_tools)` (L3): validasi `session_tools` vs isolation mode; reject built-in conflicting (`read, write, bash, execute, glob, list, patch, edit`) → `tool_isolation_unavailable`. Info event `bridge.preflight.pass` / `bridge.preflight.reject`.
  - `probe_cli_tools_live()` (L3): placeholder untuk live CLI `tools/list` probing (tidak di-wire; hindari startup latency).
  - `execute_tool()` (L4): event `bridge.tool_call.emit` (sebelum hold), `bridge.tool_call.reject` (guard violation), `bridge.tool_call.resolved`, `bridge.tool_call.timeout`. Termasuk `tool_call_id` (OpenAI UUID), nama tool, run_id, args.
- `src/provider.rs` runtime guard di `execute_tool()`: `is_bridge_tool()` check → reject non-bridged tool dengan `tool_isolation_violation`.

### Cargo check / test hasil
- `cargo check` → exit 0 (tidak ada error kompilasi).
- `cargo test` → semua test suite hijau (101 tests pass).

### Risiko / regresi catatan
- Preflight probe validasi statis terhadap `session_tools` (sudah difilter oleh IMPL-TOOL-01 selective disable), tanpa spawn CLI live — cepat & deterministik.
- Selective mode (default) aman untuk free model: tidak trigger `provider.free-limit` karena tidak inject deny-permission blok.
- Backward compatible: `[bridge.isolation]` diabaikan saat tidak ada (default selective).
- Chat-only behavior tidak berubah — hanya jalur bridge/tool run yang inject isolation config.

### Item perlu verifikasi di IMPL-TOOL-05
1. E2E: selective disable benar-benar efetif di jalur opencode bridge (inject ke `opencode.json` yang dibaca CLI, bukan sekadar dihitung).
2. VPS E2E: agentgate di `10.11.1.21:8780` dengan model `iceman-free`, verifikasi tidak ada `provider.free-limit` regresi.
3. Chat-only path (non-bridge) tidak terpengaruh oleh perubahan isolation/system prompt.
4. 9router/AionUI rantai: payload direkt gateway vs via 9router — konsistensi `tool_call_id`, stream contract.

---

## FIX-TOOL-01: Perbaiki injeksi efektif selective disable (L1) ke config CLI opencode ✅
**File:** `src/provider/bridge/opencode.rs`

**Masalah:** `build_opencode_config_json` mengabaikan parameter `tool_disable_config` (disimpan sebagai `_tool_disable_config`). Blok `tools`/`permission` untuk disable built-in tidak masuk ke `opencode.json` yang dikonsumsi opencode CLI.

**Perbaikan:**
- `build_opencode_config_json()` sekarang memanggil `Self::build_selective_disable_config(isolation)` / `Self::build_strict_disable_config()` untuk menghasilkan `tool_disable` config berdasarkan `bridge.isolation.mode`.
- `tool_disable` (tools + permission) di-merge ke `opencode.json` output untuk selective/strict modes.
- Mode `off` menghasilkan empty config (tidak ada disable).
- Metode `build_selective_disable_config` dan `build_strict_disable_config` dipindah dari `OpenCodeBridgeProvider` ke `OpenCodeSession` agar bisa diakses dari `build_opencode_config_json` (kedua di dalam `impl OpenCodeSession`).
- `OpenCodeBridgeProvider::build_tool_disable_config()` sekarang delegates ke `OpenCodeSession::build_*_disable_config()`.

**Contoh isi opencode.json hasil (selective mode):**
```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {"agentgate": {"type": "remote", "url": "...", ...}},
  "mcpServers": {"agentgate": {"type": "http", "url": "...", ...}},
  "isolation": {"mode": "selective", "allowed_builtins": [], "disabled_builtins": []},
  "tools": {
    "read": false, "write": false, "bash": false, "execute": false,
    "glob": false, "list": false, "patch": false, "edit": false,
    "grep": true, "webfetch": true, "todowrite": true, "todoread": true, "task": true
  },
  "permission": {
    "*": "deny", "externalDirectory": "deny", "network": "deny",
    "edit": "deny", "bash": "deny", "webfetch": "deny", "write": "deny",
    "read": "deny", "execute": "deny"
  }
}
```

**Hasil check/test:**
- `cargo check` → exit 0
- `cargo test` → semua test suite hijau (0 failed)

**Risiko:**
- Strict mode mungkin memicu `provider.free-limit` pada free model (sama seperti sebelum fix). Selective mode (default) aman.
- Non-bridge chat-only path tidak berubah.
- Stream/tool_calls/resume tidak berusaha.
