# TOOL-HANDLING.md — Mode Tool, Permission & MCP Bridge di `agentgate` (v2)

> Dokumen pendamping `plan.md`. Detail teknis bridge ada di **`spec_tooling.md`**; manajemen sesi di **`spec_session.md`**.
> Provider yang dicakup: **opencode, gemini, claude (Claude Code), ollama**.

---

## 0. Perubahan dari v1

| Topik | v1 | v2 (dokumen ini) |
|---|---|---|
| Request ber-`tools[]` | Diabaikan (`ignore`) | **Dijembatani lewat MCP Bridge** → client yang mengeksekusi |
| Request tanpa `tools[]` | Passthrough (tool bawaan CLI aktif di B) | **`chat_only`** (default): semua tool bawaan dimatikan |
| "Mode B: emulation" (eksperimental) | Parsing teks | Diganti **MCP Bridge** (suspend/resume), prioritas tinggi |
| Ollama | Tidak dibahas | Kelas `model-api`: **native proxy** tanpa bridge |
| Permission mode | Untuk semua request | Hanya relevan untuk **passthrough** (opt-in) |
| Opsi config | `on_client_tools = ignore/reject/emulate` | `on_client_tools = bridge/reject/ignore` + `routing.no_tools_policy` |
| Sesi | Tidak diatur (tiap request = sesi baru) | **Session manager** (`spec_session.md`): sticky/replay, identitas header-first + anchor, multi-user via *subject* |
| Perantara 9router | Tidak dibahas | Dikonfirmasi: *Compress tool output (RTK)*, *Compress context (Headroom)*, *Compress LLM output* — memengaruhi hasil tool dan pencocokan sesi |

### Insiden yang memicu perubahan
Dari AionUi (A) → 9router → agentgate (B) → opencode, prompt "buat file" **membuat file di B**, bukan di A.
Penyebab: `tools[]` dari client diabaikan, sehingga opencode memakai tool `write` bawaannya yang berjalan di B. Dengan v2, tool bawaan dimatikan dan tool client dijembatani, sehingga eksekusi terjadi **hanya di A**.

---

## 1. Konsep Dasar

### 1.1 Model hanya menghasilkan teks
Model tidak punya akses disk/terminal. Ia mengeluarkan teks terstruktur yang berarti "panggil fungsi X dengan argumen Y" (*tool calling*).

### 1.2 Agent loop & pembagian peran
```
1. kirim pesan + daftar tool ke MODEL
2. MODEL balas: tool_call(write_file, {...})
3. EKSEKUTOR (harness/client) mengeksekusi di mesinnya
4. hasil dikirim balik ke MODEL
5. ulangi sampai MODEL menjawab teks biasa
```
| Peran | Tanggung jawab |
|---|---|
| **Model** | Memutuskan kapan, tool apa, argumen apa |
| **Eksekutor** | Menyediakan tool, memeriksa izin, **mengeksekusi di mesinnya** |

### 1.3 Siapa eksekutornya?
| Skenario | Eksekutor | Lokasi efek |
|---|---|---|
| CLI agent dipakai langsung | CLI (harness) | Mesin tempat CLI berjalan (B) |
| Client OpenAI dengan `tools[]` (AionUi, Cline, SDK) | **Client** | Mesin client (A) |
| `agentgate` + CLI **tanpa** bridge | CLI | **B** ← sumber insiden |
| `agentgate` + CLI **dengan** bridge | **Client** | **A** ✅ |

---

## 2. Tiga Mode Operasi

```
                       ┌─ provider ollama ────────────► NATIVE   (proxy, tool_calls apa adanya)
request masuk ─ classify┤
                       ├─ role:tool di ujung messages ─► RESUME / REPLAY
                       ├─ tools[] ada ─────────────────► BRIDGE  (MCP Bridge, tool di client)
                       └─ tools[] kosong ──────────────► CHAT_ONLY (default) | PASSTHROUGH (opt-in)
```

### 2.1 Mode BRIDGE *(default untuk request ber-`tools[]`)*
`agentgate` menjadi **server MCP** untuk CLI. Tool dari `tools[]` dipasang ke CLI sebagai tool MCP; tool bawaan CLI dimatikan.

```
Client(A)         9router      agentgate(B)                    CLI(B)
 │ POST {tools}      │             │ spawn CLI (MCP + no builtin) ─►│
 ├──────────────────►├────────────►│◄── MCP tools/call write_file ──┤  (DITAHAN)
 │◄── SSE: tool_calls[write_file], finish=tool_calls               │  (CLI TERPARKIR)
 │ [eksekusi LOKAL di A]                                           │
 │ POST {messages + role:tool}                                     │
 ├──────────────────►├────────────►│ resolve panggilan MCP ────────►│ lanjut
 │◄── SSE: teks akhir, finish=stop ◄──────────────────────────────┤
```
Inti: satu **Run** (proses CLI) melayani banyak response HTTP; kelanjutan dicocokkan lewat `tool_call_id`.

### 2.2 Mode NATIVE *(ollama)*
Ollama bukan agent: tidak ada loop dan tidak ada tool bawaan. `tools[]` dan `tool_calls` diteruskan apa adanya (`/v1/chat/completions` Ollama). Tool dieksekusi client. Tidak perlu bridge.

### 2.3 Mode CHAT_ONLY dan PASSTHROUGH *(request tanpa `tools[]`)*
| Mode | Perilaku | Kapan |
|---|---|---|
| **CHAT_ONLY** *(default)* | CLI jalan di cwd kosong, **semua tool bawaan mati**; hanya tanya-jawab | Rantai client→gateway; tidak ada efek samping di B |
| **PASSTHROUGH** *(opt-in per workspace)* | CLI memakai tool bawaannya **di B** dengan `permission_mode` | Jika memang ingin agent bekerja di mesin B (mis. repo di B) |

Jika Anda memang ingin "agent bekerja di B", gunakan PASSTHROUGH dengan workspace B yang eksplisit; jika ingin "agent bekerja di A", gunakan BRIDGE.

---

## 3. Penanganan `tools[]` per Provider

| Provider | Kelas | Request ber-`tools[]` | Cara |
|---|---|---|---|
| opencode | cli-agent | **BRIDGE** | MCP remote lewat config inline per-run; tool bawaan dimatikan |
| gemini | cli-agent | **BRIDGE** | `mcpServers.httpUrl` di settings per-run; `excludeTools` bawaan |
| claude | cli-agent | **BRIDGE** | `--mcp-config` + `--strict-mcp-config` + `--tools ""` |
| ollama | model-api | **NATIVE** | Proxy; isi `id` tool call bila kosong |

Bila bridge belum lulus preflight untuk suatu CLI (versi tak dikenal / isolasi gagal): ikuti `on_client_tools`:
- `reject` *(default)* → HTTP 501 `tool_bridge_unsupported` (gagal keras, tidak ada efek di B)
- `ignore` → **tidak disarankan** (mengulang insiden)

---

## 4. Permission Mode (khusus PASSTHROUGH)

Mode abstrak diterjemahkan ke flag/config tiap CLI. **Flag berubah antar versi — verifikasi dengan `--help`.**

| Mode | Arti |
|---|---|
| `read-only` | Hanya baca/analisis (default) |
| `auto-edit` | Edit otomatis, bash dibatasi |
| `full-auto` | Semua otomatis — hanya di sandbox terisolasi |

| CLI | `read-only` | `auto-edit` | `full-auto` |
|---|---|---|---|
| opencode | `permission` edit/bash = `deny` | `edit = allow`, `bash = deny/ask` | semua `allow` |
| gemini | auto-approve mati; non-interaktif menolak tool berisiko | `--approval-mode auto_edit` | `--yolo` / `yolo` |
| claude | `--permission-mode plan` atau `--allowedTools` hanya tool baca | `--permission-mode acceptEdits` | `--dangerously-skip-permissions` |
| ollama | n/a (tidak ada tool bawaan) | n/a | n/a |

Aturan:
- Mode `ask` **tidak disediakan** (tidak ada manusia di terminal CLI → hang).
- Bila izin tidak bisa dijamin → **fail closed** (tolak, jangan longgarkan).
- Izin dan workspace **hanya dari config server**, tidak dari body request.
- Permission mode **tidak berlaku** pada BRIDGE/CHAT_ONLY: tool bawaan mati total.

---

## 5. Isolasi (BRIDGE & CHAT_ONLY)

| Kontrol | Keterangan |
|---|---|
| **Tool bawaan mati** | Per CLI lewat config/flag (lihat `spec_tooling.md` §12) |
| **cwd kosong per Run** | `{data_dir}/runs/{run_id}/`, tidak ada file proyek B |
| **Config disuntik per-run** | Tidak mengubah config pengguna; MCP milik pengguna tidak ikut (`--strict-mcp-config` dsb.) |
| **Konteks dinonaktifkan** | AGENTS.md / CLAUDE.md / GEMINI.md / memori — sejauh CLI mengizinkan |
| **Env bersih** | Hanya PATH, HOME, token login CLI, `NO_COLOR`, `TERM=dumb`, `LANG` UTF-8 |
| **Guard runtime** | Event CLI memakai tool non-bridge → Run dihentikan, error `tool_isolation_violation` |
| **Preflight isolasi** | Uji sekali per versi CLI (cache): coba tulis file → harus gagal; tool probe harus terpanggil |
| **Bridge loopback + token** | `127.0.0.1`, session id & token acak per Run, tolak `Origin` |

---

## 6. MCP, Skill & File Konteks

| Hal | Dikelola oleh | Perilaku di mode BRIDGE/CHAT_ONLY |
|---|---|---|
| MCP server milik pengguna di CLI | Config CLI | **Tidak dimuat** (hanya MCP bridge milik gateway) |
| Skill / instruksi CLI | Folder/config CLI | Tidak dimuat bila bisa dimatikan |
| `AGENTS.md` dll. | Workdir | Tidak terbaca (cwd kosong) |

Di PASSTHROUGH: MCP/skill CLI ikut aktif; pastikan `permission_mode` mencakup tool MCP bertulis, dan `agentgate doctor` menampilkan MCP yang terdeteksi.

---

## 7. Menampilkan Aktivitas

Di BRIDGE, aktivitas tool **sudah terlihat natural** oleh client lewat `tool_calls`. Opsi `show_tool_activity` hanya relevan untuk PASSTHROUGH:
`hide` *(default)* | `inline` | `reasoning` | `extension` (event SSE kustom).

---

## 8. Model Ancaman

| Ancaman | Mitigasi |
|---|---|
| Eksekusi di B tanpa disadari (insiden) | Default `chat_only`; `bridge` dengan tool bawaan mati; `reject` bila tak bisa dijamin |
| CLI diam-diam memakai tool bawaan | Guard runtime + preflight isolasi |
| Prompt injection (hasil tool/web) | Hanya client yang mengeksekusi; gateway memperlakukan hasil sebagai data; batasi ukuran |
| Akses tak sah ke gateway | Loopback default; API key bila non-loopback; cek `Origin`/`Host` |
| Akses tak sah ke Bridge | Token & session per Run, loopback |
| Eskalasi lewat request | Mode/workspace/config bridge hanya dari server |
| Kebocoran konteks B (AGENTS.md, memori) | cwd kosong, konteks dimatikan, uji P11 |
| Banyak run terparkir | `max_parked_runs`, TTL, per-key limit, `max_steps` |
| Perantara (9router) mengubah hasil tool / riwayat (RTK, Headroom) | Kompres tool output aman bila hasil tak harus persis; **matikan Compress context** pada rute ini atau andalkan ID sesi eksplisit + anchor; uji E2E |
| Banyak pengguna satu API key (lintas-pengguna) | `subject` wajib; tanpa subject → replay-only (`spec_session.md` §5.4) |
| Tool tambahan dari perantara (`headroom_retrieve`) | Diperlakukan sebagai tool biasa; `max_steps`, `max_tools`; uji siapa yang mengeksekusinya |
| Pelanggaran ToS vendor | Penggunaan pribadi/lokal; tinjau ToS tiap CLI |

Prinsip: **fail closed, least privilege, config server menang atas request, client satu-satunya eksekutor tool.**

---

## 9. Konfigurasi (`agentgate.toml`)

```toml
[routing]
no_tools_policy = "chat_only"     # chat_only | passthrough
on_client_tools = "bridge"        # bridge | reject | ignore

[bridge]
enabled         = true
park_ttl_secs   = 600
call_ttl_secs   = 600
max_parked_runs = 8
max_steps       = 64

[bridge.guard]
enforce         = true
preflight_probe = true

[provider.opencode]
enabled = true
bridge  = true

[provider.gemini]
enabled = true
bridge  = true
models  = ["gemini-2.5-pro", "gemini-2.5-flash"]

[provider.claude]
enabled = true
bridge  = true
models  = ["sonnet", "opus", "haiku"]

[provider.ollama]
enabled  = true
base_url = "http://127.0.0.1:11434"
num_ctx  = 16384

# hanya dipakai bila no_tools_policy = "passthrough"
[workspace.default]
path            = "."
permission_mode = "read-only"
```
Konfigurasi lengkap: `spec_tooling.md` §14.

---

## 10. Perubahan Kode

```
src/
├── bridge/            # server MCP loopback, skema, penamaan
├── run/               # Run, registry, batching, classify, prompt
├── session/           # SessionManager, ledger, resolusi sesi, store (lihat spec_session.md)
├── permission/        # PermissionMode (PASSTHROUGH saja), workspace
├── provider/
│   ├── bridge_trait.rs  # BridgeProvider: preflight + spawn_bridged
│   ├── opencode.rs  gemini.rs  claude.rs
│   └── ollama.rs        # model-api native proxy
└── openai/
    ├── tools.rs         # tools/tool_choice, generator tool_call_id
    └── sse_tools.rs     # chunk tool_calls
```

Alur `POST /v1/chat/completions`:
1. Auth + cek `Origin`; turunkan **tenant** (API key) dan **subject** (key per-user / header / `user`).
2. `classify(req)` → `Native | Resume | Replay | NewBridge | NewChatOnly`, lalu **resolusi sesi** (`spec_session.md` §5–§7): resume via `tool_call_id` → ID eksplisit → anchor → hash chain (fallback). Mode multi-user tanpa subject → replay-only.
3. `Native` → proxy ollama.
4. `Resume` → cocokkan `tool_call_id`, lanjutkan Run terparkir, stream hasil.
5. `NewBridge` → preflight → normalisasi tool → spawn CLI (config per-run) → tunggu batch → emit `tool_calls`, parkir Run.
6. `NewChatOnly` → CLI tanpa tool, atau PASSTHROUGH bila diizinkan workspace.

---

## 11. Pengujian

| Tes | Tujuan |
|---|---|
| **R1 — regresi insiden** | "Buat file" lewat fake-client: file ada **hanya** di A, **tidak ada** file baru di B |
| T1–T20 di `spec_tooling.md` §16 | Siklus tool, streaming, paralel, replay, guard, ollama, soak |
| Preflight isolasi | CLI tidak bisa menulis file di luar bridge |
| Uji manual AionUi→9router→agentgate | Tool call tampil di AionUi; file terbentuk di A |
| RTK 9router ON/OFF | Menilai distorsi `tool_result` |
| Windows & Linux CI | Stdin, kill process tree, `.cmd` |

---

## 12. Milestone

| Milestone | Isi |
|---|---|
| **BR0** | `chat_only` default + `on_client_tools=reject` (hentikan insiden) |
| **BR1** | Ollama native proxy (baseline cepat validasi rantai AionUi→9router) |
| **BR2** | `spike-bridge`: verifikasi P0–P11 untuk opencode/gemini/claude |
| **BR3** | Core bridge opencode (non-stream) |
| **BR4** | Streaming, multi-langkah, paralel, replay |
| **BR5** | claude |
| **BR6** | gemini |
| **BR7** | Hardening: guard, preflight, metrik, soak, Windows |
| **BR8** | Opsional: `opencode serve`, stdio shim, sesi persisten, `/v1/responses` |

---

## 13. Alternatif bila Bridge Tidak Stabil

| Alternatif | Catatan |
|---|---|
| **ACP shim** (AionUi berbicara ACP ke agent) | Shim di A menjembatani stdio↔WebSocket ke `opencode acp` di B; terminal masih stub; bukan endpoint OpenAI |
| **Tool-node di A** | Daemon di A (koneksi keluar ke B) mengekspos tool lewat MCP |
| **Tanpa agent CLI** | AionUi + 9router langsung ke penyedia model; agent loop AionUi sudah menjalankan tool di A |
| **Tool di B + sinkronisasi file** | PASSTHROUGH + git/SSHFS/Syncthing |

---

## 14. Keputusan Desain (Ringkas)

1. **Client adalah satu-satunya eksekutor tool** pada BRIDGE/NATIVE.
2. Request ber-`tools[]` → **BRIDGE**; ollama → **NATIVE**; tanpa `tools[]` → **CHAT_ONLY** secara default.
3. Tool bawaan CLI **dimatikan** + **guard runtime** + **preflight isolasi**; bila tak terjamin → **fail closed**.
4. Kelanjutan dicocokkan lewat **`tool_call_id`**, bukan riwayat; fallback **replay** bila Run hilang.
5. Config CLI **disuntik per-run**; config pengguna tidak disentuh.
6. Izin, workspace, dan konfigurasi bridge **hanya dari config server**.
7. Flag/config CLI bersifat **versioned** (`compat/*.json`), diverifikasi lewat spike.
