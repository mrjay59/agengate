# Audit Tooling Spec (terkini) — AgentGate tool bridge & isu rantai AionUI/9router

| | |
|---|---|
| **Tanggal** | 2026-11-12 (pembaruan investigasi dari audit 2026-11-11) |
| **Scope** | Kepatuhan `docs/spec_tooling.md` pada jalur tool-bridge (AionUI/klien → 9router opsional → AgentGate → opencode/CLI), isolasi prompt, dan gejala “Unknown tool” / tool_calls tidak dapat digunakan |
| **Baseline kode/deploy** | Repo `main @ 75e3d53` (pushed). Lokal `127.0.0.1:8780` & VPS `10.11.1.21:8780` menjalankan binary **75e3d53** (health 200) setelah perbaikan 404 + strip system prompt |
| **Referensi** | `docs/spec_tooling.md` (v0.2), `docs/audit_tooling_spec_2026-11-11.md` (baseline 11-11), laporan **VERIFY-01** (Testing-Gateway, `01a1c9a8-3032…`, e7cf6c7), `tests/session_tools_reference.md` |

> **Ringkasan:** (1) **404 invoke model 9router dinamis teratasi** (e7cf6c7). (2) **Kebocoran `role:system` klien ditemukan & diperbaiki di AgentGate** pada jalur native CLI prompt builder (75e3d53) selain perbaikan bridge/native sebelumnya (2b75349, e7cf6c7); verifikasi perilaku pada model 9router/opencode masih memerlukan uji A/B terkontrol karena pengaruh perilaku model/agent. (3) **Tool bridge client-executed AgentGate berfungsi** (emit `tool_calls` + terminasi + resume terbukti pada 9router, bypass 9router). (4) Gejala **“Unknown tool”/tool mati di AionUI** pada rantai penuh terutama terkait **eksekusi di klien/relay 9router-AionUI dan batasan topologi** (VPS tak dapat akses file lokal Windows), bukan semata AgentGate; bagian rantai di luar repo tetap di luar cakupan patch AgentGate.

---

## 1. Topologi & implikasi (mengapa “tidak bisa baca lokal” via VPS)

- **Klien AionUI (Windows, workdir user)** adalah eksekutor tool client (`spec §1, §2.1, §4.3, §11.3`).
- **AgentGate (VPS 10.11.1.21, Linux)** menjembatani CLI di workdir sesi terisolasi (`{data_dir}/sessions/{id}/work`, mis. `/tmp/agentgate/sessions/.../work`) dan **tidak** mengakses `C:\...` milik klien.
- **Konsekuensi:** saat pakai gateway VPS, tool yang dieksekusi di sisi gateway/session VPS tidak dapat membaca file lokal Windows. Akses file lokal mensyaratkan eksekusi di **mesin klien** (AionUI) dengan relay `tool_calls` ↔ `role:"tool"` yang benar, atau gateway berjalan lokal di Windows.

---

## 2. Temuan investigasi (trace) — kebocoran system-prompt

### 2.1 Jalur & perbaikan yang diterapkan (AgentGate, in-repo)

| Jalur | Sebelum | Perbaikan | Commit/Deploy |
|---|---|---|---|
| Bridge/CLI (opencode bridge, tool-capable) | `role:system` dirender/diteruskan via `--system` | Hapus forwarding `--system` & `system_prompt`; `render_conversation` skip `System` | **2b75349** (deploy lokal+VPS) |
| Native Ollama HTTP | `to_ollama_request` meneruskan semua role termasuk `system` | Filter `MessageRole::System` | **e7cf6c7** |
| Ollama CLI | `messages_to_prompt` menyertakan `System:` | Skip `System` | **e7cf6c7** |
| **Native CLI prompt builder (chat-only, `CliProviderImpl`)** | Prompt dibangun `messages.iter().filter_map(content).join("\n")` → **menyertakan konten system** | Filter `MessageRole::System` sebelum join | **75e3d53** (deploy lokal+VPS) |

**Bukti kebocoran (sebelum 75e3d53, lokal, `agg/9router/ag-poli`):** request dengan `role:system` instruksi Prancis menghasilkan respons yang meng-echo teks system+user (isi instruksi Prancis) → konten system ikut terkirim ke CLI/model. Setelah 75e3d53, uji token unik (`SECRET_TOKEN_*` dalam system) **tidak** muncul di respons (indikasi strip efektif pada jalur tersebut); uji instruksi Prancis pada opencode/9router tetap menunjukkan respons Prancis dalam percobaan tertentu → **ambigu** (lihat §2.2).

### 2.2 Sisa/ambigu (perlu verifikasi terkontrol)
- **VERIFY-01 (e7cf6c7):** GAP “system prompt diteruskan” dilaporkan pada ag-poli/free-forever (A/B bahasa). **75e3d53** menutup kebocoran yang teridentifikasi di native CLI prompt builder (jalur chat-only opencode) — **re-verifikasi A/B pasca 75e3d53** pada 9router diperlukan.
- **Perilaku model/opencode agent:** opencode CLI dapat memuat konteks/skill/instruksi sendiri (teramati: tanpa system klien, respons kadang berupa konten bantuan OpenCode/skill). Perbedaan respons A/B tidak selalu ekuivalen dengan “client system diteruskan”; butuh bukti payload/prompt aktual atau uji token/marker deterministik.
- **`preserve_client_system_prompt`:** ada di config namun **belum di-wire** end-to-end (default strip aktif; opsi preserve belum berfungsi).

### 2.3 Di luar AgentGate (rantai)
- **Merge prompt harness AionUI (Windows) dengan prompt opencode** pada hop 9router/AionUI → di luar repo AgentGate; dapat menyebabkan kolisi instruksi/tool.

---

## 3. Tool bridge, MCP, dan “Unknown tool” — temuan

### 3.1 Apa yang AgentGate lakukan (sesuai desain client-executed)
- Untuk request ber-`tools[]` (bridge), AgentGate mendaftarkan MCP session, menyuntikkan config ke `opencode.json` (isolasi L1 + MCP server `agentgate` via HTTP/stdio-shim), menjalankan CLI di workdir sesi terisolasi, **menahan** `tools/call` MCP, dan **memancarkan `tool_calls` OpenAI** ke klien untuk dieksekusi; klien mengirim `role:"tool"` + `tool_call_id` → resume/resolve (`src/bridge.rs`, `src/provider/bridge/opencode.rs`, `src/provider.rs`, `src/api.rs`, `src/service.rs`).
- **Bukti (VERIFY-01, bypass 9router, e7cf6c7):** `ag-poli` lokal/VPS & `free-forever` VPS — tool flow bridged (contoh `agentgate_read_file`) menghasilkan delta `tool_calls` + `finish_reason:"tool_calls"` + `[DONE]` dan **resume OK**. Stream/terminasi/non-regresi OK; tidak ada `provider.free-limit` semu.

### 3.2 Sumber “Unknown tool” (analisis)
| Sumber potensial | Letak | Keterangan |
|---|---|---|
| **Rewrite/pemetaan ID tanpa korelasi** (`call-…` ↔ `call_…`) atau forward ke registry salah | 9router/AionUI (di luar repo) | Memutus kecocokan `tool_call_id` ↔ hasil; gejala “Unknown tool” di klien. **N/A fix di AgentGate** kecuali normalisasi defensif. |
| **Eksekusi tool bukan di klien yang tepat** | Rantai/klien | Tool client-executed harus dijalankan AionUI di Windows; jika 9router/AionUI tidak merutekan/mengeksekusi dengan registry yang benar, gagal. |
| **Built-in opencode vs tool bridged (L1)** | AgentGate/CLI | Selective mode menonaktifkan konflik (`bash/read/write/patch/glob/list/execute/edit`) dan **mengizinkan** `grep, webfetch, todowrite, todoread, task`. VERIFY-01: `grep` muncul pada **resume lokal free-forever** → perlu pastikan apakah di luar skema bridged (gap) atau tool aman terizinkan; konsistensi guard pada **resume** perlu diperkuat. |
| **MCP bridge tidak terpasang/terlihat** | Config/session | Perlu verifikasi per sesi: `opencode.json` tertulis (mcp `agentgate`), CLI terhubung, tools terdaftar; indikasi dari VERIFY-01 (tool_calls bridged muncul) → bridge aktif pada kasus yang diuji. |
| **Topologi/path** | Lingkungan | “Baca lokal” via VPS gagal karena file Windows tidak ada di VPS — sering tampak seperti kegagalan tool, namun akar masalahnya akses/environment. |

> **Catatan:** string “Unknown tool” tidak ditemukan sebagai error utama di source AgentGate; di kode muncul pada konteks `tool_view` (reasoning) sebagai fallback tampilan — gejala “Unknown tool” yang dilaporkan pengguna kemungkinan berasal dari opencode/AionUI/registry rantai, bukan emisi langsung gateway.

### 3.3 Streaming & kontrak API (diverifikasi)
- SSE tool-calls: delta `tool_calls` incremental, `finish_reason:"tool_calls"`, terminasi `[DONE]` (v0.2.5+). `/v1/models` OpenAI-compatible. Model 9router ter-resolve via `resolve_provider` (fix 404, e7cf6c7).

---

## 4. Status kepatuhan ringkas (`docs/spec_tooling.md`)

| Area | Status | Catatan |
|---|---:|---|
| G1 Klien eksekutor tool client | ⚠️ | Bridge/hold/resume ada; akses file lokal hanya bila klien mengeksekusi di host-nya (VPS ≠ Windows klien). |
| Kontrak `tools`/`tool_calls`/`role:tool`/stream SSE | ✅ (gateway, terverifikasi) | Emit/terminasi & resume OK pada uji bypass 9router. |
| Cwd sesi terisolasi di host AgentGate | ✅ | `sessions/{id}/work`. |
| Isolasi built-in tools (L1) | ✅ | Selective/strict: semua konflik (bash/read/write/patch/glob/list/execute/edit + **opencode_list_mcp_resources**/list_mcp_resources) dinonaktifkan di config dan preflight probe; safe subset (`grep/webfetch/todowrite/todoread/task`) diizinkan; guard runtime (`is_bridge_tool`) konsisten pada initial + resume/continuation (FIX-L1-01). |
| Isolasi system prompt klien | ⚠️ | Strip default aktif (FIX-PROMPT-01); re-verifikasi A/B pasca fix masih diperlukan per model 9router. |
| E2E rantai AionUI→9router→AgentGate (tool di workdir Windows) | ❌ Belum terbukti | Bergantung perbaikan 9router/AionUI (ID mapping, relay, prompt, tool-view) + topologi eksekusi klien. |

---

## 5. Rekomendasi & tindak lanjut

**In-repo (AgentGate), prioritas:**
1. **Re-verifikasi & tutup residual system-prompt** pada 9router/opencode pasca 75e3d53 (uji token/marker + A/B terkontrol; trace prompt/argumen aktual bila masih terpengaruh); wire `preserve_client_system_prompt` bila diperlukan.
2. **Hardening L1 pada resume/lanjutan**: pastikan tool yang di-emit/diterima sesuai skema bridged & policy; evaluasi perlakuan `grep` (diizinkan selective vs kebutuhan bridge-only); jaga guard/preflight konsisten.
3. **Re-uji E2E** (`tools→tool_calls→finish_reason tool_calls+[DONE]→resume`, stream, tanpa 404, tanpa `provider.free-limit` semu) pada `9router/ag-poli` & `9router/free-forever` (lokal & VPS) setelah (1)-(2).

**Di luar repo (9router/AionUI/klien):**
4. Pemetaan ID `tool_call` stabil call↔result, relay `tool_calls`/`role:tool` ke eksekutor/registry yang benar, isolasi prompt per-hop, mapping reasoning/tool-view. Untuk akses file lokal Windows, eksekusi tool harus di klien Windows (bukan gateway VPS).

**Investigasi yang ditugaskan (Testing-Gateway):** reproduksi terkontrol “Unknown tool”/ketidakmampuan pakai tool_calls pada rantai yang relevan, identifikasi asal (opencode built-in, MCP bridge tidak terdaftar/terlihat, ID/registry mismatch, atau eksekusi di host yang salah), dengan bukti (request/response SSE, `tool_calls`/`tool_call_id`, resume, log/jejak yang tersedia). Lihat task board.
