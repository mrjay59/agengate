# SPEC_TOOLING.md — Spesifikasi Tool Bridge (MCP Bridge) `agentgate`

| | |
|---|---|
| **Status** | Draft v0.2 (siap untuk spike/prototipe; disinkronkan dengan `spec_session.md`) |
| **Tanggal** | 2026-11-02 |
| **Cakupan provider** | `opencode`, `gemini`, `claude` (Claude Code), `ollama` |
| **Dokumen terkait** | `plan.md` (rencana umum), `tool-handling.md` (kebijakan izin & mode), `spec_session.md` (manajemen sesi) |
| **Perubahan v0.2** | cwd **per-Session** (§11.3); replay = rotate Session (§10.5); Run gagal ⇒ Session `tainted` (§10.9); tool tambahan dari perantara (§8.7); konfirmasi RTK/Headroom di rantai 9router (§19 R5, R13) |
| **Konvensi** | **MUST / SHOULD / MAY** mengikuti makna RFC 2119. Tanda ⚠️ = *harus diverifikasi pada versi CLI terpasang* sebelum dianggap benar. |

---

## 0. Ringkasan Eksekutif

`agentgate` menerima request OpenAI-compatible berisi `tools[]` dari client (mis. AionUi di perangkat A). Agent CLI di perangkat B (opencode / gemini / claude) memiliki agent loop sendiri dan, tanpa jembatan, mengeksekusi tool bawaannya **di B**.

**Solusi:** `agentgate` menjadi **server MCP** bagi CLI. Tool milik client dipasang ke CLI sebagai tool MCP, sementara tool bawaan CLI dimatikan. Ketika CLI memanggil tool MCP, agentgate **menahan** panggilan itu, menerbitkan `tool_calls` OpenAI ke client, menunggu client mengeksekusi di A, lalu **melanjutkan** CLI saat hasil tool kembali di request berikutnya.

Untuk **ollama** (bukan agent, tidak punya loop) tidak perlu jembatan: `tools`/`tool_calls` diteruskan secara native.

---

## 1. Latar Belakang & Insiden

### 1.1 Topologi

```
Perangkat A (user)                         Perangkat B (server)
┌───────────────────┐   ┌──────────┐   ┌─────────────────────────────┐
│ AionUi            │   │ 9router  │   │ agentgate  ──spawn──► CLI   │
│  agent loop (UI)  ├──►│ (router, ├──►│ (OpenAI API)         opencode│
│  eksekusi tool    │   │ translasi│   │                      gemini │
│  LOKAL di A       │◄──┤ format)  │◄──┤                      claude │
└───────────────────┘   └──────────┘   │ ollama (HTTP lokal B)       │
                                       └─────────────────────────────┘
```

### 1.2 Insiden yang teramati

Saat prompt "buat file ..." dikirim dari AionUi, **file justru terbentuk di perangkat B** (di workdir proyek B), bukan di A.

### 1.3 Akar masalah (rantai sebab)

1. AionUi (agent bawaan) mengirim `tools[]` dan mengharapkan balasan `tool_calls` yang akan ia eksekusi lokal.
2. `agentgate` v1 memakai kebijakan `on_client_tools = "ignore"` → field `tools` **dibuang**.
3. CLI (mis. opencode) dijalankan dengan tool bawaan aktif (write/edit/bash) dan `cwd` = workdir B.
4. Model memutuskan memakai tool `write` bawaan CLI → **dieksekusi di B**.
5. CLI mengembalikan teks "file sudah dibuat" → AionUi hanya menerima teks, tidak pernah menerima `tool_calls`.

Ini **bukan bug implementasi**, melainkan konsekuensi desain *passthrough*: agent loop berada di CLI, bukan di client.

### 1.4 Mitigasi segera (sebelum bridge selesai)

- Set `routing.no_tools_policy = "chat_only"`: CLI dijalankan dengan **semua tool bawaan dimatikan** dan cwd kosong (tidak ada eksekusi di B).
- Untuk request yang membawa `tools[]` selama bridge belum ada: `on_client_tools = "reject"` (HTTP 501 dengan pesan jelas) — lebih baik gagal keras daripada diam-diam bertindak di B.

---

## 2. Tujuan, Non-Tujuan, Prinsip

### 2.1 Tujuan
- **G1.** Tool yang diminta model dieksekusi **hanya oleh client** (A), tidak pernah oleh B.
- **G2.** Client OpenAI standar (SDK, AionUi, Cline, dll.) tidak perlu dimodifikasi.
- **G3.** Mendukung opencode, gemini, claude (Claude Code), dan ollama.
- **G4.** Berfungsi lintas rantai perantara (9router) dan lintas Linux/Windows.
- **G5.** Streaming (SSE) dan non-streaming, termasuk multi-langkah dan tool paralel.
- **G6.** *Fail closed*: bila isolasi tool tidak bisa dijamin, request ditolak.

### 2.2 Non-Tujuan (v1)
- Mengeksekusi tool di B atas nama client.
- Tunneling jaringan B→A atau daemon tool-node (dibahas sebagai alternatif di §19).
- Dukungan gambar/biner pada hasil tool (hanya teks).
- Emulasi tool-calling untuk model tanpa dukungan tool (ollama: opsional, off).
- `/v1/responses` API (fase berikutnya).

### 2.3 Prinsip Desain
1. **Client adalah satu-satunya eksekutor tool.**
2. **ID sebagai kunci kebenaran**: kelanjutan dicocokkan lewat `tool_call_id`, bukan tebakan riwayat.
3. **Konfigurasi per-run, tidak global**: config CLI disuntik per proses, tidak mengubah file config pengguna.
4. **Jangan percaya, verifikasi**: bila stream event CLI menunjukkan pemakaian tool non-bridge → hentikan run.
5. **Degradasi anggun**: bila run yang diparkir hilang, fallback ke *replay* riwayat.

---

## 3. Terminologi

| Istilah | Arti |
|---|---|
| **Client** | Aplikasi pemanggil API OpenAI (AionUi di A) |
| **Harness / CLI agent** | opencode, gemini, claude: program dengan agent loop sendiri |
| **Bridge** | Server MCP yang di-host `agentgate`, diakses CLI lewat loopback |
| **Bridged tool** | Tool dari `tools[]` client yang dipasang ke CLI via Bridge |
| **Run** | Satu eksekusi CLI (proses) untuk melayani satu "giliran" percakapan, bisa mencakup banyak langkah tool |
| **Parked run** | Run yang prosesnya hidup tetapi menunggu hasil tool dari client |
| **Batch** | Sekelompok panggilan tool paralel yang diterbitkan dalam satu `tool_calls` |
| **Resume** | Request lanjutan berisi `role: "tool"` yang melanjutkan Parked run |
| **Replay** | Memulai Run baru dengan riwayat diratakan ke prompt (fallback) |
| **Native provider** | Provider berbasis HTTP (ollama) yang tool-calling-nya sudah native |

---

## 4. Arsitektur

### 4.1 Tiga bidang (plane)

| Bidang | Tanggung jawab | Modul |
|---|---|---|
| **Chat API plane** | Menerima request OpenAI, klasifikasi, SSE/JSON, error | `server/routes_chat.rs`, `openai/*` |
| **Bridge plane** | Server MCP loopback, menahan `tools/call`, menjembatani ke Run | `bridge/*` |
| **Provider runtime plane** | Spawn/kill CLI, config per-run, parsing event, ollama HTTP | `provider/*` |

### 4.2 Komponen

```
                 ┌────────────────────── agentgate ──────────────────────┐
HTTP /v1/chat    │  routes_chat ─► classify ─► RunManager                 │
(from 9router) ──►                               │   ▲                    │
                 │                  new/resume    │   │ RunEvent           │
                 │                               ▼   │                    │
                 │            ┌─────────── Run (tokio task) ───────────┐  │
                 │            │ CliProcess ◄─ spawn/kill ─ Provider     │  │
                 │            │ PendingCalls: tool_call_id → oneshot    │  │
                 │            └───────────────▲────────────────────────┘  │
                 │                            │ hold/resolve               │
                 │   Bridge (MCP, Streamable HTTP, 127.0.0.1)              │
                 │   POST /_bridge/mcp/{session}  ◄───── CLI (MCP client) │
                 └─────────────────────────────────────────────────────────┘
```

### 4.3 Urutan satu siklus tool

```
Client(A)      9router     agentgate(B)                      CLI(B)
  │ POST chat {tools}        │                                  │
  ├──────────►├─────────────►│ classify=New; create Run         │
  │           │              │ start Bridge session             │
  │           │              ├─ spawn CLI (config: MCP+no builtin)►│
  │           │              │◄─── MCP initialize / tools/list ──┤
  │           │              │◄─── MCP tools/call write_file ────┤  (ditahan)
  │◄── SSE: delta.tool_calls[write_file#call_x], finish=tool_calls │
  │   (response #1 selesai; CLI tetap hidup = PARKED)            │
  │ [eksekusi di A]                                              │
  │ POST chat {messages: ..., {role:tool, tool_call_id:call_x}}  │
  ├──────────►├─────────────►│ classify=Resume(call_x)          │
  │           │              │ resolve pending → MCP result ────►│ lanjut
  │◄── SSE: delta.content "Selesai…", finish=stop ◄──────────────┤
```

### 4.4 Multi-langkah dan paralel

```
Run:  text → [call A, call B]  ──(batch#1)──► finish=tool_calls   (response #1)
      client: result A, result B
Run:  resume → text → [call C] ──(batch#2)──► finish=tool_calls   (response #2)
      client: result C
Run:  resume → text final ───────────────────► finish=stop        (response #3)
```
Satu **percakapan** = banyak **response HTTP**; satu **Run** (proses CLI) melayani semuanya selama tidak kedaluwarsa.

---

## 5. Kelas Provider & Matriks Kapabilitas

| Provider | Kelas | Agent loop | Tool bawaan | Mode untuk request ber-`tools` | Mekanisme |
|---|---|---|---|---|---|
| `opencode` | `cli-agent` | di CLI | ada (dimatikan di bridge) | **bridge** | MCP remote via `OPENCODE_CONFIG_CONTENT` |
| `gemini` | `cli-agent` | di CLI | ada (dimatikan) | **bridge** | MCP `httpUrl` via settings sistem/per-run |
| `claude` | `cli-agent` | di CLI | ada (dimatikan) | **bridge** | `--mcp-config` + `--strict-mcp-config` |
| `ollama` | `model-api` | **tidak ada** | tidak ada | **native** (proxy) | `/v1/chat/completions` Ollama |

Matriks fitur bridge (tujuan v1):

| Fitur | opencode | gemini | claude | ollama |
|---|---|---|---|---|
| Streaming teks | ✅ | ✅ | ✅ | ✅ |
| Tool paralel | ✅ ⚠️ | ✅ ⚠️ | ✅ ⚠️ | ✅ (tergantung model) |
| Event terstruktur untuk guard | JSON ⚠️ | `stream-json` | `stream-json` | n/a |
| Matikan tool bawaan | config `tools`/`permission` ⚠️ | exclude/core ⚠️ | `--tools ""` ⚠️ | n/a |
| Isolasi MCP dari config user | env inline ⚠️ | system settings path ⚠️ | `--strict-mcp-config` ✅ | n/a |
| Sesi persisten antar-giliran | opsional (v2) | opsional (v2) | opsional (v2) | stateless |

---

## 6. Pemilihan Mode

### 6.1 Tabel keputusan (dievaluasi berurutan)

| # | Kondisi | Mode | Tindakan |
|---|---|---|---|
| 1 | Provider kelas `model-api` (ollama) | **native** | Proxy langsung (§12.5) |
| 2 | Pesan terakhir `role:"tool"` dan semua `tool_call_id` ditemukan di registry | **resume** | Lanjutkan Parked run (§10.4) |
| 3 | Pesan terakhir `role:"tool"` tetapi id tidak ditemukan | **replay** | Run baru + riwayat diratakan (§10.5) |
| 4 | `tools[]` tidak kosong dan `tool_choice != "none"` | **bridge** | Run baru dengan Bridge (§10.1) |
| 5 | `tools[]` kosong / `tool_choice == "none"` | **chat_only** *(default)* atau **passthrough** | Sesuai `routing.no_tools_policy` |

> **Catatan keputusan:** default `no_tools_policy = "chat_only"` karena rantai AionUi→9router→agentgate adalah skenario utama dan efek samping di B tidak diinginkan. `passthrough` (tool bawaan CLI aktif di B, aturan di `tool-handling.md` §5) hanya untuk workspace yang secara eksplisit mengaktifkannya.

### 6.2 Provider yang tidak mendukung bridge
Jika CLI belum diverifikasi bridge-capable pada versi terpasang dan request membawa `tools[]` → ikuti `on_client_tools`:
`reject` (default, HTTP 501) | `ignore` (tidak disarankan; mengulang insiden).

---

## 7. Kontrak API OpenAI

### 7.1 Field request

| Field | Penanganan |
|---|---|
| `model` | `provider/model`; menentukan provider dan model |
| `messages[]` | Role `system`, `developer`, `user`, `assistant` (+`tool_calls`), `tool`. `developer` diperlakukan seperti `system` |
| `tools[]` | Hanya `type:"function"`. Tipe lain (`custom`, dll.) → HTTP 400 `unsupported_tool_type` |
| `tool_choice` | Lihat §8.5 |
| `parallel_tool_calls` | Lihat §8.5 |
| `stream`, `stream_options.include_usage` | Didukung |
| `temperature`, `top_p`, dll. | Diabaikan (kecuali ollama: diteruskan) |
| `max_tokens` / `max_completion_tokens` | Dipotong di gateway untuk cli-agent; diteruskan untuk ollama |
| `n` | `>1` → HTTP 400 |
| `response_format` | cli-agent: diabaikan + header `X-Agentgate-Ignored`; ollama: diteruskan |

### 7.2 Klasifikasi request (`classify`)

```rust
enum Classified {
    Native,                                   // provider model-api
    Resume { calls: Vec<ToolResultIn> },      // semua id ada di registry
    Replay { history: Vec<Message> },         // role:tool tapi id hilang
    NewBridge { tools: Vec<ToolDef> },        // tools[] ada
    NewChatOnly,                              // tidak ada tools
}
```

Algoritma `trailing_tool_messages`: ambil rangkaian pesan `role:"tool"` berurutan di **akhir** `messages`. Jika kosong → bukan resume.

### 7.3 Respons non-stream dengan tool call

```json
{
  "id": "chatcmpl-ag-7f3a",
  "object": "chat.completion",
  "created": 1762080000,
  "model": "opencode/anthropic/claude-sonnet-4",
  "choices": [{
    "index": 0,
    "message": {
      "role": "assistant",
      "content": null,
      "tool_calls": [{
        "id": "call_7f3a0001a1b2c3d4",
        "type": "function",
        "function": { "name": "write_file", "arguments": "{\"path\":\"hello.txt\",\"content\":\"halo\"}" }
      }]
    },
    "finish_reason": "tool_calls"
  }],
  "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }
}
```
- `arguments` **MUST** berupa string JSON (bukan objek).
- Jika CLI menghasilkan teks sebelum tool call, isi ke `content` (boleh non-null bersama `tool_calls`).

### 7.4 Respons stream dengan tool call

Urutan chunk (semua chunk berbagi `id`, `created`, `model`):

```
data: {"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}], ...}

data: {"choices":[{"index":0,"delta":{"content":"Saya akan membuat file."},"finish_reason":null}], ...}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_7f3a0001a1b2c3d4","type":"function","function":{"name":"write_file","arguments":""}}]},"finish_reason":null}], ...}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"hello.txt\",\"content\":\"halo\"}"}}]},"finish_reason":null}], ...}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}], ...}

data: [DONE]
```

Aturan:
- `index` pada `tool_calls` = posisi dalam batch (0,1,2…), bukan global.
- `id` dan `name` hanya pada chunk pertama tiap tool call; chunk berikutnya hanya `arguments` (potongan).
- Karena MCP memberi argumen utuh, gateway **MAY** mengirim `arguments` dalam satu chunk, atau memecah pada batas karakter UTF-8 (≤ 512 byte per chunk).
- Batch dengan N panggilan: kirim semua `tool_calls` index 0..N-1 sebelum chunk `finish_reason`.
- Jika `stream_options.include_usage`, kirim chunk usage (estimasi) sebelum `[DONE]`.

### 7.5 Request lanjutan (resume)

```json
{
  "model": "opencode/anthropic/claude-sonnet-4",
  "stream": true,
  "tools": [ { "...": "sama seperti sebelumnya" } ],
  "messages": [
    { "role": "user", "content": "buat file hello.txt" },
    { "role": "assistant", "content": null, "tool_calls": [
        { "id": "call_7f3a0001a1b2c3d4", "type": "function",
          "function": { "name": "write_file", "arguments": "{\"path\":\"hello.txt\",\"content\":\"halo\"}" } } ] },
    { "role": "tool", "tool_call_id": "call_7f3a0001a1b2c3d4", "content": "OK: 4 bytes ditulis" }
  ]
}
```
- Gateway **MUST** mencocokkan `tool_call_id`; **MUST NOT** bergantung pada kesamaan riwayat.
- Isi `messages` sebelum pesan assistant terakhir **diabaikan** pada resume (konteks sudah ada di Run).

### 7.6 Error (format OpenAI)

```json
{ "error": { "message": "...", "type": "invalid_request_error", "param": null, "code": "tool_bridge_unsupported" } }
```

| Situasi | HTTP | `code` |
|---|---|---|
| `tools[]` ada tapi provider tidak bridge-capable | 501 | `tool_bridge_unsupported` |
| Isolasi tool tidak bisa dijamin (fail closed) | 503 | `tool_isolation_unavailable` |
| `tool_call_id` kedaluwarsa dan replay dimatikan | 409 | `tool_run_expired` |
| Hasil tool sebagian (batch tidak lengkap) setelah grace | 200 + lanjut dengan error-result | — |
| Terlalu banyak Parked run | 429 | `too_many_parked_runs` |
| Tool type tidak didukung | 400 | `unsupported_tool_type` |
| CLI belum login / crash | 502 | `provider_error` |
| CLI memakai tool non-bridge (guard) | 502 | `tool_isolation_violation` |

Error **setelah** stream dimulai: kirim satu chunk `data: {"error":{...}}`, lalu `data: [DONE]`.

---

## 8. Pemetaan Tool

### 8.1 Definisi: OpenAI function → MCP tool

```
OpenAI: { "type":"function", "function": { "name", "description", "parameters", "strict" } }
MCP   : { "name", "description", "inputSchema" }
```
- `parameters` → `inputSchema` (setelah normalisasi §8.2).
- `strict` diabaikan.
- Tambahkan anotasi opsional: `annotations.title = name`.

### 8.2 Normalisasi skema

Aturan (MUST):
1. Root `inputSchema` harus `{"type":"object"}`. Jika `parameters` kosong/tidak ada → `{"type":"object","properties":{}}`.
2. Hapus `$schema`; inline `$ref` lokal (`#/$defs/...`) sampai kedalaman 8; tolak `$ref` eksternal.
3. Batasi ukuran skema per tool (default 64 KB) dan jumlah tool (default 128).

Profil per CLI (`schema_profile`), SHOULD diterapkan:
| Profil | Dipakai | Aturan tambahan |
|---|---|---|
| `permissive` | opencode, claude | Tidak ada perubahan lain |
| `strict_gemini` | gemini | Buang `additionalProperties`, `patternProperties`, `oneOf/anyOf/allOf` yang tak bisa disederhanakan (ambil cabang pertama + catat peringatan); `format` yang tak dikenal dibuang; enum tipe campuran → string ⚠️ |

### 8.3 Penamaan & alias

Sisi MCP, CLI menerima nama **mentah** dari `tools/list`; prefix (`mcp__ag__…`, `ag_…`) ditambahkan CLI di sisinya sendiri. Maka `params.name` pada `tools/call` yang diterima Bridge = nama yang kita deklarasikan (tanpa prefix).

Prefix hanya relevan untuk:
- batas panjang nama total di sisi model (Claude/Gemini ≈ 64 karakter) dan
- mengenali tool bridge vs non-bridge pada event stream (guard §13).

Aturan alias:
1. `budget = profile.max_total_name_len - len(profile.prefix(server_name))`.
2. Jika `name` cocok regex `^[A-Za-z0-9_-]+$` dan `len ≤ budget` → pakai apa adanya.
3. Selain itu → alias `t{NN}_{sanitized[..budget-4]}` (NN = indeks 2 digit). Sanitasi: ganti karakter di luar regex dengan `_`.
4. Tabel `NameMap { alias ↔ original }` disimpan per Run; **semua** panggilan masuk diterjemahkan kembali ke nama asli sebelum menjadi `tool_calls`.
5. Duplikat setelah sanitasi → sufiks `_2`, `_3`.

Contoh nama total (`server_name = "agentgate"`):
- Claude: `mcp__agentgate__<name>` → prefix 16 → budget 48.
- opencode: `agentgate_<name>` → prefix 10 → budget 54.
- Gemini: ⚠️ ukur nyata (`mcp_agentgate_<name>` atau `agentgate__<name>`).

### 8.4 `tool_call_id`

Format: `call_` + 16 hex = 21 karakter, mis. `call_7f3a0001a1b2c3d4`.
- 8 hex pertama = `run_id` pendek, 2 hex = nomor batch, 2 hex = indeks, 4 hex = acak.
- Sesuai regex `^[A-Za-z0-9_-]{1,64}$` (aman diterjemahkan 9router ke format Anthropic/Gemini).
- Memungkinkan deteksi "run milik instance ini tapi sudah dibersihkan" vs "id asing".

### 8.5 `tool_choice` dan `parallel_tool_calls`

| Nilai | Perilaku |
|---|---|
| `"auto"` / tidak ada | Semua tool diekspos |
| `"none"` | Mode **chat_only** (tidak ada tool diekspos) |
| `"required"` | Tool diekspos + instruksi sistem: "Kamu harus memanggil minimal satu tool." (best-effort) |
| `{"type":"function","function":{"name":X}}` | Hanya tool X diekspos + instruksi memanggil X |
| `parallel_tool_calls:false` | Mode **serialisasi**: satu tool call per giliran (lihat §10.3) |

### 8.6 Hasil tool → MCP result

Input: pesan `role:"tool"` (`content` string atau array bagian).

```json
{ "content": [ { "type": "text", "text": "<isi>" } ], "isError": false }
```
- Array bagian: gabungkan bagian `text`; bagian non-teks diganti `[konten non-teks dihilangkan]`.
- `isError` default `false` (OpenAI tidak punya flag). Opsional `tools.error_heuristic = true`: jika isi diawali `Error:`/`error:` → `isError=true`.
- Batas ukuran hasil (default 256 KB); lebih → dipotong + penanda `[dipotong N byte]`.
- Hasil yang datang untuk `tool_call_id` yang tidak sedang menunggu → diabaikan + log `warn`.

### 8.7 Tool tambahan dari perantara (mis. `headroom_retrieve`)

Perantara di hulu (9router dengan Headroom) dapat **menambahkan tool** ke `tools[]` — misalnya tool pengambil data asli untuk kompresi reversibel — dan menyingkat isi pesan.
- Gateway memperlakukan tool tersebut **sama seperti tool client lain**: dipasang ke bridge, dapat dipanggil model, dan diteruskan sebagai `tool_calls`. Tidak ada daftar tool yang dikenali/ditolak secara khusus.
- Siapa yang mengeksekusinya bukan urusan gateway: bila perantara meng-*intercept* panggilan itu, ia akan mengirim **request resume tambahan** ke gateway (jalur resume normal berbasis `tool_call_id`). ⚠️ Perilaku ini harus dibuktikan di rantai nyata (§16.4).
- `max_steps` dan `max_parked_runs` melindungi dari putaran tool tak berujung.
- Untuk identitas sesi, nama tool pada `session.ignore_tools_in_hash` tidak ikut `tools_hash` (`spec_session.md` §6.6).
- Hasil tool yang dikompresi perantara diteruskan **apa adanya** ke CLI (gateway tidak mendekompresi).

---

## 9. Server MCP Bridge

### 9.1 Transport & endpoint

- **Streamable HTTP** (⚠️ pastikan revisi spesifikasi MCP yang dipakai CLI), endpoint tunggal:
  `POST http://127.0.0.1:{port}/_bridge/mcp/{session_id}`
- `session_id`: UUID v4 acak per Run; berlaku hanya selama Run hidup.
- Bind **hanya loopback**, terpisah dari listener publik API bila `server.host` bukan loopback (listener bridge selalu `127.0.0.1`, port acak atau tetap via `bridge.port`).
- `GET` pada endpoint → `405` (tidak ada stream server→client mandiri).
- `DELETE` → akhiri sesi MCP (idempotent, `200`).

### 9.2 Autentikasi
- Header `Authorization: Bearer {bridge_token}`; token 256-bit acak per Run, dibandingkan *constant-time*.
- Token & session id disuntik ke CLI lewat config per-run; tidak pernah dicatat di log.
- Permintaan dengan `Origin` header → ditolak (CLI bukan browser).
- Tolak jika `session_id` tidak aktif → `404`.

### 9.3 Metode JSON-RPC

| Metode | Perilaku |
|---|---|
| `initialize` | Balas `protocolVersion` = versi client jika didukung (`2024-11-05`, `2025-03-26`, `2025-06-18`, `2025-11-25`), selain itu versi terbaru yang didukung; `capabilities.tools.listChanged=false`; `serverInfo` |
| `notifications/initialized` | `202 Accepted` tanpa body |
| `ping` | `{}` |
| `tools/list` | Daftar bridged tools (tanpa paginasi; abaikan `cursor`) |
| `tools/call` | **Ditahan** sampai hasil client tiba (§9.5) |
| `notifications/cancelled` | Batalkan panggilan terkait (§9.6) |
| `resources/*`, `prompts/*`, `logging/*` | `resources/list` & `prompts/list` → hasil kosong; lainnya `-32601` |
| lainnya | `-32601 Method not found` |

### 9.4 Contoh pertukaran

```json
// → initialize
{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"opencode","version":"x.y.z"}}}
// ←
{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"agentgate-bridge","version":"0.1.0"}}}

// → tools/list
{"jsonrpc":"2.0","id":1,"method":"tools/list"}
// ←
{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"write_file","description":"Tulis file di workstation user","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}}]}}

// → tools/call (ditahan)
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"hello.txt","content":"halo"}}}
// ← (setelah client mengirim hasil)
{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"OK: 4 bytes ditulis"}],"isError":false}}
```

### 9.5 Menahan `tools/call` (hold)

1. Validasi `arguments` terhadap `inputSchema` (JSON Schema validator). Gagal → balas **segera** `isError:true` dengan pesan validasi (model bisa memperbaiki; tidak membebani client).
2. Daftarkan `PendingCall{ mcp_request_id, alias→original name, args, oneshot::Sender }` di Run.
3. Kirim `RunEvent::ToolCallRequested` ke batcher (§10.3).
4. Respons HTTP untuk `tools/call` **SHOULD** berupa `text/event-stream`:
   - kirim komentar `: keepalive` tiap 15 detik agar proxy/CLI tidak memutus koneksi;
   - saat hasil tiba, kirim satu event `message` berisi JSON-RPC response lalu tutup.
5. Jika CLI hanya menerima `application/json`, balas JSON saja dan tahan koneksi (tanpa keepalive) — ⚠️ ukur apakah ada timeout idle di CLI.
6. Timeout internal = `bridge.call_ttl_secs` (default 600). Habis → balas `isError:true` "timeout menunggu hasil dari client".

### 9.6 Pembatalan
- `notifications/cancelled{requestId}` dari CLI → hapus PendingCall; bila batch sudah terkirim ke client, hasil yang datang kemudian diabaikan.
- Koneksi HTTP `tools/call` terputus dari sisi CLI → perlakukan sebagai batal.

### 9.7 Fallback: stdio shim
Bila CLI bermasalah dengan MCP-HTTP (header auth, OAuth auto-discovery, Windows):
- `agentgate mcp-shim --session {id} --token {t}` = proses stdio MCP kecil yang diluncurkan CLI sebagai server `command`-based, meneruskan JSON-RPC ke Bridge lewat loopback HTTP / pipe bernama.
- Dipakai sebagai transport alternatif: `bridge.transport = "http" | "stdio_shim"` per provider.

---

## 10. Run Lifecycle & Registry

### 10.1 State machine

```
            spawn ok            tools/call masuk (batch terkumpul)
 Starting ──────────► Running ─────────────────────────────► AwaitingTools
     │                  │  ▲                                     │
     │ spawn gagal      │  │ semua hasil batch diterima          │ hasil tiba (resume)
     ▼                  │  └──────────────── Resuming ◄──────────┘
   Failed               │ CLI selesai (teks final)       TTL habis / dibatalkan
                        ▼                                       ▼
                       Done                                  Expired / Cancelled
```

| State | Ada HTTP response terbuka? | Proses CLI | Keterangan |
|---|---|---|---|
| Starting | ya | sedang start | Menunggu `initialize` MCP |
| Running | ya | aktif | Mengalirkan teks ke client |
| AwaitingTools | **tidak** (response #n sudah ditutup) | hidup, menunggu | Terparkir; hitung TTL |
| Resuming | ya (response #n+1) | aktif lagi | Hasil tool diteruskan |
| Done | tutup | exit 0 | Bersihkan sumber daya |
| Failed | tutup | exit ≠0 / crash | Kirim error |
| Expired | — | dibunuh | TTL habis |
| Cancelled | — | dibunuh | Client disconnect saat Running / pembatalan |

### 10.2 Struktur data

```rust
pub struct RunId(pub [u8; 4]);                 // 8 hex

pub struct Run {
    pub id: RunId,
    pub provider: ProviderId,
    pub model: String,
    pub state: Mutex<RunState>,
    pub events_rx: Mutex<Option<mpsc::Receiver<RunEvent>>>,   // diambil oleh handler yang "attach"
    pub pending: DashMap<McpReqId, PendingCall>,              // panggilan MCP yang menunggu
    pub awaiting: Mutex<Option<Batch>>,                       // batch yang sudah diterbitkan ke client
    pub name_map: NameMap,
    pub tool_defs_hash: u64,
    pub cancel: CancellationToken,
    pub permit: OwnedSemaphorePermit,
    pub created_at: Instant,
    pub parked_since: Mutex<Option<Instant>>,
}

pub struct Batch {
    pub seq: u8,
    pub calls: Vec<IssuedCall>,                // tool_call_id, original_name, mcp_req_id
}

pub struct PendingCall {
    pub mcp_req_id: McpReqId,
    pub issued: Option<String>,                // tool_call_id jika sudah diterbitkan
    pub args: serde_json::Value,
    pub resolver: oneshot::Sender<ToolOutcome>,
}

pub enum ToolOutcome { Ok(String), Err(String) }

pub enum RunEvent {
    Text(String),
    Reasoning(String),
    ToolCalls(Batch),
    Usage(Usage),
    Done(FinishReason),
    Error(RunError),
}

/// registry global
pub struct RunRegistry {
    by_run: DashMap<RunId, Arc<Run>>,
    by_call: DashMap<String /*tool_call_id*/, RunId>,
}
```

### 10.3 Algoritma batching (tool paralel)

Parameter: `batch.window_ms` (default 150), `batch.max_calls` (default 16).

1. Panggilan MCP pertama masuk saat state `Running` → mulai timer `window_ms`.
2. Panggilan lain yang tiba sebelum timer habis masuk batch yang sama (hingga `max_calls`).
3. Timer habis → buat `Batch`, generate `tool_call_id`, emit `RunEvent::ToolCalls(batch)`; state → `AwaitingTools`.
4. Handler HTTP menerjemahkan ke SSE/JSON (§7.3–7.4), mengirim `finish_reason: "tool_calls"`, menutup response; **proses CLI tidak disentuh**.
5. Panggilan yang tiba **setelah** batch diterbitkan (saat Parked) → masuk antrean `late`; diterbitkan sebagai batch berikutnya segera setelah resume (di dalam response lanjutan).
6. **Mode serialisasi** (`parallel_tool_calls:false`): terbitkan hanya 1 panggilan per batch; sisanya tetap pending dan diterbitkan pada batch berikutnya di response lanjutan.

### 10.4 Resume

Input: daftar `(tool_call_id, content)` dari pesan `role:"tool"` di ujung `messages`.

1. Lookup `RunRegistry.by_call[id]` untuk setiap id; semua harus menunjuk **Run yang sama** dan **batch yang sedang menunggu**. Jika tidak → Replay (§10.5).
2. Ambil `events_rx` (hanya satu consumer; jika sudah diambil → `409 run_busy`).
3. Selesaikan `oneshot` tiap panggilan dengan `ToolOutcome::Ok(content)`.
4. **Hasil parsial:** jika sebagian id batch belum datang, tunggu `resume.partial_grace_ms` (default 0 = tidak menunggu). Id yang hilang diselesaikan dengan `Err("hasil tool tidak diterima dari client")` agar CLI tidak menggantung.
5. State → `Resuming` → `Running`; stream `RunEvent` berikutnya ke response saat ini.
6. Hapus entri `by_call` untuk batch itu.

**Idempotensi:** request resume yang sama diterima dua kali (retry 9router/client) → request kedua menemukan batch sudah selesai → `409 tool_already_resolved`... **kecuali** jika response pertama putus sebelum selesai dan `resume.allow_retry = true`: gateway menyimpan buffer event terakhir (maks 256 KB) dan memutar ulang. Default v1: tidak didukung (dokumentasikan).

### 10.5 Fallback Replay

Dipakai bila id tidak ditemukan (restart gateway, TTL habis, id asing) dan `resume.replay = true` (default):
1. **Rotate Session** (`spec_session.md` §8.3): `cli_gen += 1`, CLI session baru; ledger direset ke riwayat request (client = sumber kebenaran).
2. Bangun Run **baru** (bridge baru) dengan seluruh riwayat diratakan ke prompt (§11), termasuk pasangan `tool_calls` ↔ hasil tool sebelumnya.
3. Respons mengalir seperti request baru. Konteks CLI hilang tetapi percakapan tetap berlanjut.
4. Log `warn: replay_fallback{reason}`; header respons `X-Agentgate-Replay: 1` dan `X-Agentgate-Replay-Reason`.

### 10.6 TTL, batas, pembersihan

| Parameter | Default | Keterangan |
|---|---|---|
| `bridge.park_ttl_secs` | 600 | Maks waktu Parked menunggu request resume |
| `bridge.call_ttl_secs` | 600 | Maks satu `tools/call` ditahan |
| `bridge.max_parked_runs` | 8 | Total Parked run |
| `bridge.max_runs_per_key` | 4 | Per API key |
| `bridge.max_run_lifetime_secs` | 3600 | Batas mutlak umur Run |
| `bridge.max_steps` | 64 | Maks batch tool per Run (pencegah loop) |

Pembersihan (wajib di semua jalur: Done/Failed/Expired/Cancelled): bunuh process tree → `wait()` → hapus `by_run`/`by_call` → hapus direktori run → lepas permit semaphore → hentikan sesi MCP.
Saat start gateway: sapu direktori run yatim dan PID file lama.

### 10.7 Konkurensi & race
- Satu Run = satu task yang memiliki seluruh mutasi state; handler HTTP berkomunikasi lewat channel (hindari lock lintas `.await`).
- Dua request resume bersamaan untuk Run yang sama → yang kedua mendapat `409`.
- Resume tiba tepat saat TTL habis → penentu: operasi atomik `registry.take(run_id)`; pemenang menang.
- Permit semaphore provider dipegang **selama Run hidup termasuk Parked** (`bridge.parked_holds_permit = true`), supaya Parked run tidak menembus batas konkurensi.

### 10.8 Cancel & disconnect

| Kejadian | Aksi |
|---|---|
| Client putus saat `Running`/`Resuming` | Cancel Run → kill CLI → bersihkan; **Session → `tainted`** (`spec_session.md` §8.2) |
| Client "putus" saat `AwaitingTools` | Tidak ada koneksi; hanya TTL yang berlaku |
| Request baru (bukan resume) datang untuk percakapan yang sama sementara Parked | Ikuti `session.on_parked_conflict` (`spec_session.md` §9.2): default `cancel_parked` → Run dibunuh, Session `tainted`, layani lewat rotate-replay |
| `Ctrl+C` gateway | Cancel semua Run, tunggu maks 5 detik |

### 10.9 Hubungan Run ↔ Session

| Kejadian Run | Efek pada Session (`spec_session.md`) |
|---|---|
| Selesai normal (`Done`) | Ledger di-commit (request + pesan assistant yang dikirim); state `idle` |
| Batch tool terbit (`AwaitingTools`) | Session `parked`; `tool_call_id` dicatat sebagai **anchor** |
| `Cancelled` / `Expired` / `Failed` / guard melanggar | Session `tainted` → request berikut **rotate** (replay) |
| Resume sukses | Session kembali `busy` lalu `idle` |
| Hasil tool parsial diselesaikan error-result | **Tidak** taint (struktur tetap konsisten) |

Satu Session dapat melayani banyak Run berurutan (sticky), tetapi **hanya satu Run aktif** pada satu waktu.

---

## 11. Konstruksi Prompt & Konteks

### 11.1 Sistem
Susun `system_prompt`:
```
{pesan system/developer dari client, digabung}

[Catatan agentgate]
Kamu terhubung ke workstation milik pengguna lewat tool yang tersedia. Semua aksi file/perintah
harus lewat tool tersebut. Jangan berasumsi punya akses lain. Tool dieksekusi di perangkat pengguna,
bukan di server tempat kamu berjalan.
```
Cara injeksi per CLI (§12): flag/config sistem-prompt bila ada; jika tidak, ditaruh di awal prompt.

### 11.2 Riwayat (Run baru / Replay)

Transkrip teks (urutan kronologis):
```
<riwayat>
[user] ...
[assistant] ...
[assistant memanggil tool] id=call_x nama=write_file argumen={"path":"a.txt",...}
[hasil tool] id=call_x isi=OK
...
</riwayat>

<permintaan_saat_ini>
...pesan user terakhir...
</permintaan_saat_ini>
```
- Prompt dikirim lewat **stdin** (hindari batas panjang argumen & isu `.cmd` Windows).
- Pemotongan bila melebihi `prompt.max_chars` (default 400.000): buang giliran tertua, pertahankan system + N giliran terakhir; sisipkan penanda `[riwayat lama dipotong]`.

### 11.3 Isolasi konteks (MUST)
- `cwd` = direktori **kosong per-Session** `{data_dir}/sessions/{session_id}/work/` (tidak ada file proyek B). Dibagi antar-Run dalam satu Session karena lokasi penyimpanan sesi CLI umumnya diturunkan dari cwd (`spec_session.md` §11.3). Mode `session.mode = off`/Session `ephemeral`: direktori per-Run `{data_dir}/runs/{run_id}/`.
- Nonaktifkan pemuatan file konteks (AGENTS.md / CLAUDE.md / GEMINI.md) dan memori pengguna sejauh CLI mengizinkan (⚠️ per-CLI §12); bila tidak bisa dimatikan, dokumentasikan sebagai risiko kebocoran konteks B ke model.
- Environment bersih: hanya `PATH`, `HOME`/`USERPROFILE`, variabel login CLI, `NO_COLOR=1`, `TERM=dumb`, `LANG=C.UTF-8`, plus variabel per-run.

---

## 12. Adapter per Provider

### 12.1 Antarmuka Rust

```rust
#[async_trait]
pub trait BridgeProvider: Provider {
    fn bridge_profile(&self) -> BridgeProfile;

    /// Verifikasi versi & kemampuan sebelum dipakai (fail closed)
    async fn bridge_preflight(&self) -> Result<(), ProviderError>;

    /// Jalankan CLI pada mode bridge. Mengembalikan stream event + kill handle.
    async fn spawn_bridged(&self, spec: BridgedRunSpec) -> Result<CliRun, ProviderError>;
}

pub struct BridgeProfile {
    pub server_name: String,                 // "agentgate"
    pub max_total_name_len: usize,           // 64
    pub name_prefix: fn(&str) -> String,     // "mcp__{server}__" dll.
    pub schema_profile: SchemaProfile,
    pub transport: BridgeTransport,          // Http | StdioShim
    pub supports_parallel: bool,
}

pub struct BridgedRunSpec {
    pub run_id: RunId,
    pub model: String,
    pub system_prompt: String,
    pub prompt: String,                      // dikirim via stdin
    pub bridge_url: String,                  // http://127.0.0.1:{port}/_bridge/mcp/{sid}
    pub bridge_token: SecretString,
    pub tools: Vec<BridgedTool>,             // sudah dinormalisasi + alias
    pub cwd: PathBuf,                        // direktori kosong
    pub env: Vec<(String, String)>,
    pub timeouts: Timeouts,                  // call_ttl dll.
}

pub struct CliRun {
    pub events: BoxStream<'static, CliEvent>,
    pub kill: KillHandle,
}

pub enum CliEvent {
    Text(String),
    Reasoning(String),
    ToolObserved { name: String, is_bridge: bool }, // untuk guard
    Usage(Usage),
    Exit { code: Option<i32>, stderr_tail: String },
}
```

`bridge_preflight` memeriksa: versi dalam rentang teruji, flag/config yang dibutuhkan tersedia, status login (jika bisa), dan menjalankan **uji isolasi singkat** (cache hasil per versi, lihat §17).

---

### 12.2 opencode

**Deteksi & model:** `opencode --version`; `opencode models` (cache 5 menit). Nama model gateway: `opencode/<provider>/<model>`.

**Perintah (v1: spawn per Run):**
```
opencode run --model <provider/model> [--format json] -     # prompt via stdin ⚠️ (alternatif: argumen / --file)
```
⚠️ Verifikasi: dukungan stdin, `--format json`, opsi sesi sekali pakai. Fase 2: `opencode serve` + HTTP API/SSE (menghindari cold start dan memberi event terstruktur).

**Injeksi config per-run** lewat env `OPENCODE_CONFIG_CONTENT` (lapisan config inline, prioritas tinggi) — generasi skema v1:
```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "agentgate": {
      "type": "remote",
      "url": "http://127.0.0.1:8788/_bridge/mcp/<SESSION>",
      "headers": { "Authorization": "Bearer <TOKEN>" },
      "enabled": true
    }
  },
  "tools": {
    "bash": false, "edit": false, "write": false, "read": false, "patch": false,
    "grep": false, "glob": false, "list": false, "webfetch": false,
    "todowrite": false, "todoread": false, "task": false
  },
  "permission": { "edit": "deny", "bash": "deny", "webfetch": "deny" },
  "experimental": { "mcp_timeout": 660000 },
  "share": "disabled",
  "autoupdate": false
}
```
Catatan:
- Skema **v2** opencode berbeda (`mcp.servers.<nama>`, timeout per kategori `startup/catalog/execution`, `disabled` bukan `enabled`). Adapter **MUST** memilih bentuk sesuai versi terdeteksi; versi tak dikenal → fail closed.
- Nama tool di sisi model/event: `<server>_<tool>` (mis. `agentgate_write_file`) → dipakai guard.
- Daftar nama tool bawaan yang dimatikan **berubah antar versi**: ambil dari output `opencode` yang relevan atau dari hasil uji isolasi, jangan hard-code selamanya (simpan sebagai daftar yang bisa di-override).
- ⚠️ Izin tool MCP di mode non-interaktif: pastikan tool `agentgate_*` tidak memicu prompt `ask` (set `permission` yang mengizinkan; verifikasi sintaks wildcard).
- ⚠️ `instructions`/AGENTS.md: kosongkan (`"instructions": []`) dan jalankan di cwd kosong.
- **Jangan override `XDG_DATA_HOME`** (login/auth opencode tersimpan di sana); isolasi sesi lewat cwd kosong dan penghapusan sesi setelah Run bila CLI mendukung.

**Parsing output:** mode JSON → baris event; ambil delta teks, tool event (untuk guard), usage. Mode teks → stdout mentah (fallback, tanpa guard).

**Risiko khusus:** timeout MCP bawaan terlalu pendek untuk tool yang menunggu client → `experimental.mcp_timeout` (v1) / `mcp.timeout.execution` (v2) harus ≥ `call_ttl + margin`.

---

### 12.3 gemini

**Deteksi & model:** `gemini --version`. Tidak ada perintah list model yang stabil → daftar statis di config (mis. `gemini-2.5-pro`, `gemini-2.5-flash`), dapat di-override.

**Perintah:**
```
gemini -m <model> -o stream-json -p "<instruksi singkat>"        # prompt panjang via stdin ⚠️
```
`-o stream-json` menghasilkan JSONL dengan jenis event `init`, `message`, `tool_use`, `tool_result`, `error`, `result`. Exit code: `0` sukses, `1` error, `42` input salah, `53` batas giliran terlampaui.

**Injeksi config per-run:** buat file settings per-run dan arahkan lewat `GEMINI_CLI_SYSTEM_SETTINGS_PATH` ⚠️ (alternatif: `.gemini/settings.json` di cwd run, yang merupakan lapisan proyek). Isi:

```json
{
  "mcpServers": {
    "agentgate": {
      "httpUrl": "http://127.0.0.1:8788/_bridge/mcp/<SESSION>",
      "headers": { "Authorization": "Bearer <TOKEN>" },
      "timeout": 660000,
      "trust": true
    }
  },
  "excludeTools": [
    "run_shell_command", "read_file", "write_file", "replace", "glob",
    "search_file_content", "list_directory", "read_many_files",
    "web_fetch", "google_web_search", "save_memory", "write_todos"
  ]
}
```
Catatan:
- Gemini CLI memiliki **dua generasi format settings** (legacy `coreTools/excludeTools` di root vs format baru `tools.core/tools.exclude`/pengelompokan). Adapter memilih bentuk sesuai versi; versi tak dikenal → fail closed.
- Nama tool bawaan harus diambil dari versi terpasang (⚠️ `/tools` atau dokumentasi versi tsb). Daftar di atas hanyalah contoh.
- `trust: true` pada server MCP → melewati konfirmasi per-panggilan (headless tidak bisa menjawab prompt). Pastikan `--approval-mode`/auto-approve **tidak** dinyalakan secara global (tool bawaan sudah di-exclude).
- Nama tool MCP di sisi model: ⚠️ ukur (`mcp_agentgate_<tool>` vs `agentgate__<tool>`); simpan di `BridgeProfile.name_prefix`.
- `schema_profile = strict_gemini` (§8.2).
- Konteks: set `contextFileName` ke nama yang tidak ada / cwd kosong agar GEMINI.md tidak dimuat; nonaktifkan memori (`save_memory` sudah di-exclude).
- Login: `gemini` memakai kredensial pengguna (OAuth/API key) — gateway tidak menyentuhnya.

**Parsing:** `message` (assistant) → `Text`; `tool_use` dengan nama bukan bridge → guard; `result` → `Usage`/selesai.

---

### 12.4 claude (Claude Code)

**Deteksi & model:** `claude --version`; model statis/alias (`sonnet`, `opus`, `haiku`, atau ID penuh) di config.

**Perintah:**
```
claude -p --output-format stream-json --verbose --include-partial-messages \
  --model <model> \
  --mcp-config '<JSON>' --strict-mcp-config \
  --tools "" \
  --allowedTools "mcp__agentgate" \
  --setting-sources "" \
  --system-prompt "<system>" \
  --max-turns 64 \
  --session-id <UUID>          # Run pertama Session; Run lanjutan sticky memakai --resume <UUID>
# prompt via stdin
# --no-session-persistence HANYA untuk Session ephemeral / session.mode=off (konflik dengan sticky) ⚠️
```
Catatan:
- `--output-format stream-json` **membutuhkan** `--verbose` (kalau tidak, CLI error).
- `--strict-mcp-config` memastikan hanya server dari `--mcp-config` yang dipakai (mencegah MCP milik pengguna di B ikut terpasang).
- `--tools ""` mematikan semua tool bawaan ⚠️ (verifikasi semantik string kosong pada versi terpasang; alternatif tambahan: `--disallowedTools "Bash,Edit,Write,Read,Glob,Grep,WebFetch,WebSearch,Task,NotebookEdit,TodoWrite"`).
- `--setting-sources ""` mencegah pemuatan settings user/project/local (hooks, permission, MCP, CLAUDE.md terkait) ⚠️.
- `--system-prompt` **mengganti** prompt sistem bawaan (yang mengasumsikan tool bawaan); pakai teks §11.1.
- Nama tool: `mcp__agentgate__<tool>` → budget nama = 64 − 16 = 48.

`--mcp-config`:
```json
{
  "mcpServers": {
    "agentgate": {
      "type": "http",
      "url": "http://127.0.0.1:8788/_bridge/mcp/<SESSION>",
      "headers": { "Authorization": "Bearer <TOKEN>" }
    }
  }
}
```
Env: `MCP_TOOL_TIMEOUT` ⚠️ (ms) ≥ `call_ttl + margin`; `MCP_TIMEOUT` untuk startup; `CLAUDE_CODE_*` yang relevan untuk menonaktifkan telemetri/auto-update bila diinginkan.

**Parsing `stream-json`:** event `system/init`, `assistant` (blok `text`, `tool_use`), `user` (blok `tool_result`), `stream_event` (delta parsial), `result` (akhir + usage). Blok `tool_use` dengan nama tanpa prefix `mcp__agentgate__` → pelanggaran isolasi (guard).

**Risiko khusus:** pastikan penggunaan langganan Claude lewat gateway sesuai ketentuan vendor (§19).

---

### 12.5 ollama (native, tanpa bridge)

**Kelas `model-api`:** Ollama bukan agent. Ia hanya menghasilkan teks dan `tool_calls`; **tidak ada tool bawaan dan tidak ada loop**. Karena itu client AionUi cukup berbicara OpenAI → agentgate → Ollama; semua tool tetap dieksekusi client.

**Endpoint upstream** (default `http://127.0.0.1:11434`):
| Fungsi | Endpoint |
|---|---|
| Daftar model | `GET /api/tags` (atau `GET /v1/models`) |
| Kapabilitas model | `POST /api/show {"model": "..."}` → `capabilities` (cari `"tools"`) |
| Chat | `POST /v1/chat/completions` (kompatibel OpenAI, streaming SSE, `tools`) |

**Perilaku:**
1. Nama model: `ollama/<nama:tag>`; `model` diteruskan tanpa prefix.
2. Body request diteruskan hampir apa adanya (mutasi minimal: ganti `model`, hapus field milik gateway). `temperature`, `top_p`, `seed`, `stop`, `max_tokens`, `response_format`, `tools`, `tool_choice` ikut diteruskan.
3. Respons (stream maupun non-stream) **diteruskan byte-per-byte** setelah normalisasi ringan: pastikan `id`, `created`, `model` (kembalikan nama berprefix `ollama/...`), `finish_reason: "tool_calls"` pada akhir giliran tool, dan `data: [DONE]`.
4. Jika model tidak memiliki kapabilitas `tools` dan request membawa `tools[]` → HTTP 400 `model_no_tool_support` (opsi `ollama.prompt_tools_fallback = true` mengaktifkan emulasi berbasis prompt — default off).
5. `tool_call_id`: bila upstream memberi id kosong/tidak ada, gateway **MUST** mengisi `call_<hex>` agar client dan 9router konsisten; id tidak perlu dilacak (tidak ada Run).
6. `GET /v1/models`: gabungkan dari `/api/tags`, tandai kapabilitas `tools` di field tambahan `agentgate.capabilities`.
7. **Konteks:** default `num_ctx` Ollama kecil dan dapat memotong prompt yang berisi banyak skema tool. Config `ollama.num_ctx` (diteruskan sebagai opsi) dan peringatan di `doctor` jika `< 8192` ketika `tools[]` ada. ⚠️ Cek apakah endpoint `/v1` menerima opsi ini; bila tidak, gunakan `/api/chat` native dengan konversi format.
8. `keep_alive` diteruskan bila di-set di config.

**Jalur alternatif `/api/chat` native** (bila `/v1` tidak cukup): konversi pesan, `tools` (format sama dengan OpenAI), dan hasil `tool_calls` (argumen berupa objek JSON, bukan string → `serde_json::to_string`), serta stream NDJSON → SSE.

---

### 12.6 Ringkasan perbedaan

| Aspek | opencode | gemini | claude | ollama |
|---|---|---|---|---|
| Injeksi config | `OPENCODE_CONFIG_CONTENT` | settings per-run (`GEMINI_CLI_SYSTEM_SETTINGS_PATH`) ⚠️ | flag `--mcp-config` | n/a |
| Matikan built-in | `tools`/`permission` | `excludeTools` / `tools.*` | `--tools ""` | n/a |
| Isolasi MCP user | config inline | settings sistem | `--strict-mcp-config` | n/a |
| Prefix nama tool | `agentgate_` | ⚠️ ukur | `mcp__agentgate__` | n/a |
| Format event | JSON ⚠️ | `stream-json` | `stream-json` (+`--verbose`) | SSE OpenAI |
| Prompt via | stdin ⚠️ / arg | stdin + `-p` ⚠️ | stdin | HTTP body |
| Profil skema | permissive | strict_gemini | permissive | native |
| Timeout tool | `mcp_timeout`/`execution` | `timeout` di server | `MCP_TOOL_TIMEOUT` ⚠️ | n/a |

---

## 13. Keamanan (fail-closed)

### 13.1 Jaminan yang harus ditegakkan
| ID | Jaminan | Mekanisme |
|---|---|---|
| S1 | CLI tidak menjalankan tool selain bridge | Config mematikan built-in **dan** guard runtime (S2) |
| S2 | Pelanggaran terdeteksi | Event `ToolObserved{is_bridge:false}` → kill Run, error `tool_isolation_violation` |
| S3 | CLI tidak membaca file B | cwd kosong + built-in mati + konteks dinonaktifkan |
| S4 | Bridge tak bisa diakses pihak lain | Loopback, session id + token acak per Run, tolak `Origin` |
| S5 | Client tak bisa mengeskalasi izin | `permission_mode`, workspace, config bridge hanya dari config server |
| S6 | Tidak ada kebocoran rahasia di log | Redaksi token, `Authorization`, isi argumen/hasil tool tidak dicatat default |
| S7 | Tool result tak dipercaya | Diperlakukan sebagai data; dibatasi ukuran; tidak pernah dieksekusi gateway |

### 13.2 Uji isolasi saat preflight (cache per versi CLI)
Jalankan Run uji dengan tool bridge palsu `probe_ok` dan prompt "coba tulis file `/tmp/ag_probe` lalu panggil `probe_ok`".
- **Lulus** bila: file tidak terbentuk, tidak ada event tool non-bridge, `probe_ok` terpanggil.
- **Gagal** → provider ditandai `bridge_unavailable`; request ber-`tools` ditolak (`503 tool_isolation_unavailable`).

### 13.3 Ancaman spesifik bridge
| Ancaman | Mitigasi |
|---|---|
| Argumen tool jahat dari model (path traversal, dll.) | Tanggung jawab eksekutor (client A); gateway hanya validasi skema |
| Prompt injection lewat hasil tool | Tidak bisa dicegah gateway; dokumentasikan; batasi ukuran |
| Resource exhaustion (banyak Parked run) | `max_parked_runs`, TTL, `max_steps`, per-key limit |
| Kebocoran konteks B (AGENTS.md, memori) | §11.3; uji preflight memeriksa tidak ada konteks tak terduga |
| Pengambilalihan sesi MCP | Token per Run, TTL, loopback |
| Replay hasil tool oleh pihak lain | `tool_call_id` acak + hanya dikonsumsi sekali |

---

## 14. Konfigurasi

```toml
[routing]
no_tools_policy   = "chat_only"       # chat_only | passthrough
on_client_tools   = "bridge"          # bridge | reject | ignore
default_workspace = "default"

[bridge]
enabled              = true
host                 = "127.0.0.1"    # SELALU loopback
port                 = 0              # 0 = acak
park_ttl_secs        = 600
call_ttl_secs        = 600
max_parked_runs      = 8
max_runs_per_key     = 4
max_run_lifetime_secs= 3600
max_steps            = 64
parked_holds_permit  = true
server_name          = "agentgate"

[bridge.batch]
window_ms  = 150
max_calls  = 16

[bridge.resume]
replay          = true
partial_grace_ms= 0
allow_retry     = false

[bridge.limits]
max_tools        = 128
max_schema_bytes = 65536
max_result_bytes = 262144
max_prompt_chars = 400000

[bridge.guard]
enforce          = true               # bunuh Run bila ada tool non-bridge
preflight_probe  = true

[provider.opencode]
enabled   = true
bridge    = true
transport = "http"                    # http | stdio_shim
# bin = "/path/opencode"

[provider.gemini]
enabled   = true
bridge    = true
models    = ["gemini-2.5-pro", "gemini-2.5-flash"]

[provider.claude]
enabled   = true
bridge    = true
models    = ["sonnet", "opus", "haiku"]

[provider.ollama]
enabled        = true
base_url       = "http://127.0.0.1:11434"
num_ctx        = 16384
keep_alive     = "10m"
prompt_tools_fallback = false

[workspace.default]
path            = "."                 # hanya dipakai mode passthrough
permission_mode = "read-only"
```

Prioritas: flag CLI > env (`AGENTGATE_*`) > file > default.

---

## 15. Observabilitas

**Log terstruktur** (satu baris per peristiwa, tanpa isi prompt/argumen secara default):

| Event | Field |
|---|---|
| `run.start` | `run_id, provider, model, mode, n_tools, cli_version` |
| `bridge.tools_list` | `run_id, n_tools, aliased` |
| `bridge.call_held` | `run_id, mcp_req_id, tool` |
| `batch.issued` | `run_id, seq, n_calls, tool_call_ids` |
| `run.parked` | `run_id, ttl` |
| `run.resumed` | `run_id, wait_ms, n_results` |
| `run.replay` | `reason` |
| `guard.violation` | `run_id, observed_tool` |
| `run.end` | `run_id, state, steps, duration_ms, ttft_ms, tool_wait_ms_total` |

**Metrik:** `agentgate_runs_active`, `agentgate_runs_parked`, `agentgate_tool_wait_seconds` (histogram), `agentgate_resume_total{result=ok|replay|conflict}`, `agentgate_guard_violations_total`, `agentgate_ttft_seconds`, `agentgate_cli_exit_total{code}`.

**Header respons diagnostik:** `X-Agentgate-Run`, `X-Agentgate-Mode: bridge|resume|replay|native|chat_only`.

---

## 16. Pengujian

### 16.1 Komponen tiruan
- **`fake-cli`**: skrip/binary yang berperan sebagai MCP client: `initialize` → `tools/list` → `tools/call X` (N kali, opsional paralel) → cetak teks akhir. Varian: crash, hang, memanggil tool non-bridge, mengabaikan timeout.
- **`fake-client`** (harness tes): meniru AionUi — mengirim `tools[]`, mengeksekusi `tool_calls` pada direktori sementara "A", mengirim hasil.
- **`fake-ollama`**: server HTTP yang mengembalikan stream dan `tool_calls` terjadwal.

### 16.2 Skenario wajib

| # | Skenario | Hasil yang diharapkan |
|---|---|---|
| T1 | Satu tool, non-stream | `finish_reason=tool_calls`; setelah resume → teks final `stop` |
| T2 | Satu tool, stream | Urutan chunk sesuai §7.4, `[DONE]` |
| T3 | Dua tool paralel dalam 1 batch | `tool_calls` index 0 dan 1; resume dengan 2 hasil |
| T4 | Multi-langkah (3 batch) | Satu Run melayani 3 response |
| T5 | `parallel_tool_calls:false` | Satu call per giliran, antrean tertunda |
| T6 | Resume setelah TTL habis | Replay fallback, header `X-Agentgate-Replay` |
| T7 | Gateway restart di tengah parked | Replay fallback |
| T8 | Resume parsial (1 dari 2 hasil) | Hasil hilang diselesaikan dengan error-result; Run lanjut |
| T9 | Client putus saat Running | Proses CLI mati ≤ 2 detik |
| T10 | CLI memanggil tool non-bridge | Run dihentikan, `tool_isolation_violation` |
| T11 | `tools[]` ada, provider tak bridge-capable | 501 `tool_bridge_unsupported` |
| T12 | Skema tool dengan `$ref`/`oneOf` (gemini) | Dinormalisasi, tidak ada error CLI |
| T13 | Nama tool panjang/aneh | Alias dibuat, nama asli dipulihkan pada `tool_calls` |
| T14 | Tool result besar (>256 KB) | Dipotong + penanda |
| T15 | Dua resume bersamaan | Satu sukses, satu `409` |
| T16 | `tool_choice:"none"` | chat_only, tidak ada tool terpasang |
| T17 | Request tanpa `tools` (default) | `chat_only`, tidak ada eksekusi di B |
| T18 | Ollama + tools | Proxy native, id terisi, `finish_reason=tool_calls` |
| T19 | Ollama model tanpa kapabilitas tools | 400 `model_no_tool_support` |
| T20 | Soak: 200 siklus tool berurutan | Tanpa kebocoran proses/memori/FD |
| T21 | `tools[]` memuat tool tambahan perantara (`headroom_retrieve`) | Diteruskan sebagai tool biasa; tidak error |
| T22 | Hasil tool dikompresi perantara (teks pendek/berbeda) | Diteruskan apa adanya ke CLI; resume tetap berhasil |
| T23 | Parked run dibatalkan lalu client lanjut | Session `tainted` → rotate-replay; percakapan berlanjut (lihat SS8/SS9) |

### 16.3 Regresi insiden (WAJIB lulus sebelum rilis)

> **R1.** `fake-client` kirim "buat file `hello.txt` berisi `halo`" dengan tool `write_file` ke tiap provider (opencode/gemini/claude via CLI asli atau tiruan).
> **Lulus bila:** (a) file `hello.txt` ada **hanya** di direktori A (milik fake-client), (b) **tidak ada** file baru di seluruh pohon direktori B (workdir, home, /tmp yang relevan), (c) respons akhir berupa teks setelah satu siklus tool.

### 16.4 Uji manual rantai nyata
1. B: jalankan `agentgate serve`; A: AionUi → provider OpenAI-compatible → URL 9router → model `opencode/...`.
2. Kirim "buat file test.txt". Amati: tool call muncul di UI AionUi; file terbentuk di A.
3. Periksa B: tidak ada file baru.
4. Ulangi dengan 9router **RTK token saver** ON dan OFF untuk menilai pengaruhnya pada `tool_result`.
5. Uji cancel dari UI di tengah tool call.
6. Jalankan `agentgate doctor --probe-chain`: pastikan `tool_call_id` dipertahankan, header/`user` sampai, dan catat bagaimana pesan dimodifikasi (RTK / Headroom / kompres output).
7. Ulangi uji dengan **Compress context (Headroom)** ON dan OFF; bandingkan `sticky_ratio` dan keberhasilan resume.
8. Bila Headroom aktif: periksa apakah tool `headroom_retrieve` muncul di `tools[]` dan siapa yang menjalankannya.

---

## 17. Spike (Prototipe) & Kriteria Lulus

Subcommand: `agentgate spike-bridge --cli <opencode|gemini|claude>`.
Spike menjalankan Bridge mini dengan tool `echo(text)` dan `slow(seconds)`; menjalankan CLI dengan prompt yang memaksa tool dipakai; mencetak laporan.

| ID | Pertanyaan | Lulus bila |
|---|---|---|
| P0 | Config per-run bisa disuntik tanpa menyentuh config user? | Ya, dan config user tak berubah |
| P1 | CLI mau terhubung ke MCP HTTP loopback (+header Authorization)? | `initialize` + `tools/list` diterima |
| P2 | Tool bawaan benar-benar mati? | Prompt "tulis file" tidak membuat file; tidak ada event tool non-bridge |
| P3 | `tools/call` yang ditahan 60 detik tidak di-timeout CLI? | Hasil `slow(60)` diterima CLI |
| P4 | Ditahan 600 detik? | Idem (atau tentukan timeout max aktual) |
| P5 | Panggilan paralel muncul bersamaan? | 2 `tools/call` tiba dalam jendela batch |
| P6 | Nama tool di event = prefix yang diprediksi? | Guard dapat membedakan bridge/non-bridge |
| P7 | Skema kompleks diterima? | Tool dengan `oneOf`/`$ref` terdaftar tanpa error |
| P8 | Prompt via stdin (non-ASCII, ≥100 KB) utuh di Windows & Linux? | Isi identik |
| P9 | Event terstruktur memadai (teks/tool/usage)? | Parser menghasilkan `CliEvent` benar |
| P10 | Kill process tree menghentikan semua anak? | Tidak ada proses tersisa |
| P11 | Konteks pengguna (AGENTS.md/memori) tidak bocor? | Model tidak dapat mengutip isi file tsb |

Keluaran: tabel lulus/gagal per versi CLI → disimpan sebagai `compat/{cli}-{version}.json` dan dipakai `bridge_preflight`.

---

## 18. Rencana Implementasi

| Milestone | Isi | Kriteria selesai |
|---|---|---|
| **BR0 — Mitigasi** | `no_tools_policy=chat_only`, `on_client_tools=reject`, tes R1 versi "tidak ada tulis di B" | Insiden tidak terulang |
| **BR1 — Ollama native** | Proxy `/v1/chat/completions` + model list + id fill | T18, T19; rantai AionUi→9router→agentgate→ollama memakai tool di A (baseline cepat untuk memvalidasi 9router/AionUi) |
| **BR2 — Spike** | `spike-bridge` untuk opencode/gemini/claude, laporan P0–P11 | Keputusan go/no-go per CLI |
| **BR3 — Core bridge (opencode)** | Bridge MCP, Run, registry, batching, non-stream | T1, T3, T9, T10, T13, T15 |
| **BR4 — Streaming & resume penuh** | SSE tool_calls, multi-langkah, replay | T2, T4–T8, T14, T16, T17 |
| **BR5 — claude** | Adapter + parser `stream-json` | T1–T4 untuk claude |
| **BR6 — gemini** | Adapter + profil skema + settings per-run | T1–T4, T12 untuk gemini |
| **BR7 — Hardening** | Guard, preflight, metrik, soak, Windows | T20, R1 di Linux+Windows |
| **BR8 — Opsional** | `opencode serve`, stdio shim, sesi persisten, `/v1/responses` | — |

Urutan implementasi yang disarankan: **BR0 → BR1 → BR2 → BR3 → BR4 → BR5 → BR6 → BR7**.
Manajemen sesi (`spec_session.md` SS0–SS3) dikerjakan paralel dengan BR3–BR4; SS1b (identitas berlapis) sebaiknya selesai sebelum uji rantai nyata.

---

## 19. Risiko & Pertanyaan Terbuka

| # | Risiko / pertanyaan | Dampak | Mitigasi / tindak lanjut |
|---|---|---|---|
| R1 | CLI memberi timeout pada `tools/call` yang ditahan lama | Bridge tak bisa dipakai untuk tool yang butuh persetujuan manusia | Spike P3/P4; atur timeout via config CLI; fallback: batas `call_ttl` lebih pendek + hasil error |
| R2 | Tool bawaan tidak bisa dimatikan sepenuhnya di suatu versi CLI | Eksekusi di B | Guard + preflight; fail closed |
| R3 | Format config/flag berubah antar versi | Adapter rusak | Preflight berbasis versi, `compat/*.json`, tes regresi di CI |
| R4 | Model berkinerja lebih buruk saat tool berbeda dari tool asli CLI | Kualitas turun | Deskripsi sistem §11.1; evaluasi per model; opsi memilih Ollama/API langsung |
| R5 | **Dikonfirmasi:** 9router memiliki *Compress tool output* (RTK), *Compress context* (Headroom), dan *Compress LLM output* | Hasil tool terdistorsi/terpadatkan; riwayat berubah; tool tambahan muncul | Pertimbangkan mematikan *Compress context* pada rute ini; RTK/kompres output aman bila hasil tool tidak harus persis; uji T-manual §16.4; lihat `spec_session.md` §6.6 |
| R6 | Parked run memakan sumber daya | Server lambat | TTL pendek, `max_parked_runs`, metrik |
| R7 | Client mengirim ulang riwayat berbeda dari yang diterbitkan | Resume salah sambung | `tool_call_id` otoritatif; log mismatch |
| R8 | Ketentuan layanan vendor melarang penggunaan langganan CLI lewat gateway/otomasi | Akun diblokir | Tinjau ToS tiap vendor; dokumentasikan penggunaan pribadi/lokal |
| R9 | Gemini membatasi skema fungsi | Tool ditolak | Profil `strict_gemini`, uji T12 |
| R10 | `tool_calls` stream dari gateway tidak diteruskan benar oleh 9router | Client tak menerima tool | Uji end-to-end; bandingkan langsung vs via 9router |
| R11 | Loop konfigurasi: model opencode di B menunjuk 9router yang menunjuk balik ke agentgate | Loop tak berujung | `doctor` mendeteksi base URL mengarah ke diri sendiri |
| R12 | Latensi spawn per Run | UX lambat | `opencode serve`/warm pool (BR8) |
| R13 | Headroom menambah tool (`headroom_retrieve`) dan meng-*intercept* panggilannya | Pola resume tambahan dari perantara | §8.7; uji E2E; `max_steps`; pastikan `tool_call_id` dipertahankan |
| R14 | Banyak pengguna berbagi satu API key di gateway | Risiko lintas-pengguna pada sesi | Subject wajib (`spec_session.md` §5.4); `max_runs_per_key` ditinjau per subject |

**Sudah terjawab (operator, 2026-11-03):**
- Header kustom *kemungkinan* diteruskan AionUi/9router (belum terbukti → `doctor --probe-chain`).
- 9router memang memiliki kompresi tool output (RTK), konteks (Headroom), dan output LLM.
- Banyak orang berbagi satu API key → model subject di `spec_session.md` §5.4.
- Sesi persisten antar-giliran: **diperlukan** → `spec_session.md`.

**Pertanyaan terbuka:**
1. Apakah AionUi (mode OpenAI-compatible) menjalankan `tool_calls` secara otomatis atau memerlukan konfirmasi per panggilan? (menentukan `park_ttl`)
2. Apakah 9router meneruskan `tool_calls` stream, `parallel_tool_calls`, dan `tool_call_id` utuh pada rute yang Anda pakai, termasuk saat *Compress context* aktif?
3. Apakah *Compress context* dapat dimatikan per rute/provider di 9router?
4. Apakah 9router menjalankan sendiri panggilan `headroom_retrieve` atau meneruskannya ke client?
5. Versi spesifik opencode / gemini / claude yang menjadi target dukungan (menentukan generasi skema config)?
6. Alternatif non-OpenAI: ACP shim atau tool-node di A (lihat `tool-handling.md` §13), bila bridge terbukti tidak stabil di suatu CLI.

---

## Lampiran A — Contoh Alur Lengkap (stream, 1 tool)

**Request #1** (client → agentgate):
```json
{
  "model": "opencode/anthropic/claude-sonnet-4",
  "stream": true,
  "messages": [{ "role": "user", "content": "buat file hello.txt isi: halo" }],
  "tools": [{
    "type": "function",
    "function": {
      "name": "write_file",
      "description": "Tulis file di workstation pengguna",
      "parameters": {
        "type": "object",
        "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
        "required": ["path", "content"]
      }
    }
  }]
}
```

**Response #1** (SSE):
```
data: {"id":"chatcmpl-ag-7f3a","object":"chat.completion.chunk","created":1762080000,"model":"opencode/anthropic/claude-sonnet-4","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-ag-7f3a","object":"chat.completion.chunk","created":1762080000,"model":"opencode/anthropic/claude-sonnet-4","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_7f3a0001a1b2c3d4","type":"function","function":{"name":"write_file","arguments":""}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-ag-7f3a","object":"chat.completion.chunk","created":1762080000,"model":"opencode/anthropic/claude-sonnet-4","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"hello.txt\",\"content\":\"halo\"}"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-ag-7f3a","object":"chat.completion.chunk","created":1762080000,"model":"opencode/anthropic/claude-sonnet-4","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]
```

**Request #2** (client → agentgate; dieksekusi di A):
```json
{
  "model": "opencode/anthropic/claude-sonnet-4",
  "stream": true,
  "tools": [ "...sama..." ],
  "messages": [
    { "role": "user", "content": "buat file hello.txt isi: halo" },
    { "role": "assistant", "content": null, "tool_calls": [
      { "id": "call_7f3a0001a1b2c3d4", "type": "function",
        "function": { "name": "write_file", "arguments": "{\"path\":\"hello.txt\",\"content\":\"halo\"}" } } ] },
    { "role": "tool", "tool_call_id": "call_7f3a0001a1b2c3d4", "content": "OK: 4 bytes ditulis" }
  ]
}
```

**Response #2** (SSE, dari Run yang sama):
```
data: {"id":"chatcmpl-ag-7f3b","object":"chat.completion.chunk","created":1762080012,"model":"opencode/anthropic/claude-sonnet-4","choices":[{"index":0,"delta":{"role":"assistant","content":"File hello.txt sudah dibuat."},"finish_reason":null}]}

data: {"id":"chatcmpl-ag-7f3b","object":"chat.completion.chunk","created":1762080012,"model":"opencode/anthropic/claude-sonnet-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
```

## Lampiran B — Struktur Modul Tambahan

```
src/
├── bridge/
│   ├── mod.rs            # BridgeServer (axum, 127.0.0.1)
│   ├── mcp.rs            # JSON-RPC, initialize/tools list/call
│   ├── schema.rs         # normalisasi skema + profil
│   ├── naming.rs         # alias & NameMap
│   └── shim.rs           # stdio shim (opsional)
├── run/
│   ├── mod.rs            # Run, RunState, RunEvent
│   ├── manager.rs        # RunManager, RunRegistry, TTL sweeper
│   ├── batch.rs          # batching tool paralel
│   ├── classify.rs       # klasifikasi request
│   └── prompt.rs         # transkrip riwayat & system prompt
├── provider/
│   ├── bridge_trait.rs   # BridgeProvider, BridgedRunSpec, CliRun
│   ├── opencode.rs  gemini.rs  claude.rs
│   ├── ollama.rs         # model-api native proxy
│   └── compat.rs         # compat/*.json + preflight
├── openai/
│   ├── tools.rs          # tools/tool_choice, id generator
│   └── sse_tools.rs      # chunk tool_calls
└── cli/
    └── spike.rs          # agentgate spike-bridge
```

## Lampiran C — Checklist Verifikasi Versi CLI (isi saat spike)

| Item | opencode | gemini | claude |
|---|---|---|---|
| Versi diuji | | | |
| Cara suntik config per-run | | | |
| Perintah non-interaktif + prompt via stdin | | | |
| Cara mematikan semua tool bawaan | | | |
| Nama tool bawaan (daftar versi ini) | | | |
| Format nama tool MCP di event | | | |
| Timeout MCP call (default & cara ubah) | | | |
| Perilaku `tools/call` tertahan (P3/P4) | | | |
| Panggilan paralel (P5) | | | |
| Event terstruktur tersedia | | | |
| Pemuatan konteks (AGENTS/CLAUDE/GEMINI.md, memori) | | | |
| Perilaku di Windows (.cmd, stdin, kill tree) | | | |
