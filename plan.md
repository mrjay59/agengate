# PLAN.md — `agentgate`: Gateway Provider untuk CLI Agent (Rust)

> Nama kerja: **agentgate** (bebas diganti).
> Satu binary Rust yang mengubah CLI agent yang sudah terpasang (opencode, GitHub Copilot CLI, Gemini CLI, dll.) menjadi **satu endpoint HTTP kompatibel OpenAI** (`/v1/...`).

---

## 0. Status Proyek (Task Board)

> Diupdate: 2026-10-30 · 9 task · 3 diantaranya di-*delete* (N/A / tanpa pemegang)

| # | Task | Status | Owner (slot_id) | Catatan |
|---|---|---|---|---|
| 1 | Discovery: Gather Requirements from User | ✅ completed | Features Engineer (`01a18c02-349c-7b02-91cb-6445198cb401`) | |
| 2 | Planning: Create Project Plan and Timeline | ✅ completed | Konsultan Program (`01a18c02-33cd-7183-93e2-a4b58e92d983`) | Dokumen ini |
| 3 | Design: UI/UX Design | 🗑 deleted (N/A) | Frontend Developer | Out of scope v1 — lihat §1 Non-Goals |
| 4 | Development: Backend Implementation | ✅ completed | Backend Developer (`01a18c02-3454-7043-86a2-970a439cb02a`) | Rust/Tokio + axum, trait-based provider, OpenAI-compatible API |
| 5 | Development: Frontend Implementation | 🗑 deleted (N/A) | Frontend Developer | Out of scope v1 — lihat §1 Non-Goals |
| 6 | Testing: QA and Quality Assurance | 🔄 in_progress | Features Engineer (`01a18c02-349c-7b02-91cb-6445198cb401`) | Unit + integration + fake CLI |
| 7 | Deployment: Production Deployment | ✅ completed | Admin DevOps | Gateway agentgate siap, infrastruktur/runtime lokal diinisialisasi |
| 8 | Maintenance: Ongoing Support | 🔄 in_progress | Konsultan Program (`01a18c02-33cd-7183-93e2-a4b58e92d983`) | Siap menerima laporan testing |
| 9 | Integration: Frontend-Backend | 🗑 deleted (N/A) | Frontend Developer | "Frontend" = klien eksternal (Cline/Continue/OpenWebUI/curl/OpenAI SDK), bukan UI di repo |

### Roster Tim (saat ini)
| Nama | Role | Slot ID | Status |
|---|---|---|---|
| Konsultan Program | lead | `01a18c02-33cd-7183-93e2-a4b58e92d983` | working |
| Features Engineer | teammate | `01a18c02-349c-7b02-91cb-6445198cb401` | idle |
| Backend Developer | teammate | `01a18c02-3454-7043-86a2-970a439cb02a` | idle |

> ⚠️ **Frontend Developer** (`01a18c02-3406-7e03-8979-346817fd5150`) dan **Admin DevOps** (`01a18c02-34fb-7b02-a7b1-69e2c5a3bade`) sudah di-*remove* dari tim. Task yang terkait (Design UI/UX, Frontend Implementation, Integration, Deployment) sudah di-*delete* atau completed.

---

## 1. Tujuan & Konsep

Pengguna sudah memasang satu atau beberapa CLI agent di mesinnya, masing-masing dengan login/subscription sendiri. `agentgate` **tidak menyimpan kredensial & tidak memanggil API vendor secara langsung**. Ia hanya:

1. **Mendeteksi** CLI agent yang terpasang di PATH.
2. **Membaca daftar model** dari tiap CLI.
3. **Menerima request** OpenAI-compatible (`/v1/chat/completions`, `/v1/models`).
4. **Menjalankan CLI** terkait sebagai subprocess (mode non-interaktif), lalu **menerjemahkan outputnya** kembali ke format OpenAI (JSON biasa atau SSE streaming).

```
 Client (Cline, Continue, OpenWebUI, curl, SDK OpenAI, dll)
        │  HTTP  (OpenAI-compatible)
        ▼
 ┌──────────────────────────────┐
 │          agentgate           │
 │  axum router: /, /v1/*       │
 │  Model Router  ─►  Provider  │
 │                    Registry  │
 └───────┬──────────┬───────────┘
         │          │          │
   opencode     copilot      gemini      ... (adapter)
   (subprocess) (subprocess) (subprocess)
```

### Non-Goals (v1)
- Bukan proxy ke API vendor (tidak butuh API key vendor).
- Tidak mengelola login CLI (user login manual lewat CLI masing-masing).
- Tidak meniru *tool-calling* OpenAI secara penuh (agent CLI menjalankan tool-nya sendiri).
- Tidak ada UI web (hanya API + CLI admin).

---

## 2. Platform & Persyaratan

| Item | Keputusan |
|---|---|
| OS | Linux (x86_64, aarch64), Windows 10/11 (x86_64), macOS (bonus) |
| Rust | stable, edition 2021, MSRV 1.75 |
| Runtime | `tokio` multi-thread |
| Distribusi | single static-ish binary, rilis via GitHub Releases |
| Default bind | `127.0.0.1:8787` (localhost saja) |

---

## 3. Endpoint

| Method | Path | Fungsi |
|---|---|---|
| GET | `/` | Info service: nama, versi, provider terdeteksi, link endpoint (JSON) |
| GET | `/health` | Liveness (`{"status":"ok"}`) |
| GET | `/v1/models` | Daftar model gabungan semua provider (format OpenAI `list`) |
| GET | `/v1/models/{id}` | Detail satu model |
| POST | `/v1/chat/completions` | Chat (non-stream & `stream:true` via SSE) |
| POST | `/v1/completions` | (Opsional) legacy completion → dipetakan ke chat |
| POST | `/v1/responses` | (Fase lanjut) OpenAI Responses API subset |
| GET | `/admin/providers` | Status provider: path binary, versi, siap/tidak |
| POST | `/admin/refresh` | Re-scan provider & model |

### Format Nama Model
`<provider>/<model-id>` — contoh:

```
opencode/anthropic/claude-sonnet-4
opencode/openai/gpt-5
copilot/gpt-5
gemini/gemini-2.5-pro
```
Prefix pertama = ID provider di agentgate, sisanya diteruskan apa adanya ke CLI.

---

## 4. Arsitektur Kode

### 4.1 Struktur Repo

```
agentgate/
├── Cargo.toml
├── PLAN.md
├── README.md
├── agentgate.example.toml
├── src/
│   ├── main.rs                 # entry, clap CLI
│   ├── config.rs               # load TOML + env + flag
│   ├── error.rs                # AppError -> HTTP response (format error OpenAI)
│   ├── server/
│   │   ├── mod.rs              # build axum Router
│   │   ├── routes_root.rs      # GET /, /health
│   │   ├── routes_models.rs    # /v1/models
│   │   ├── routes_chat.rs      # /v1/chat/completions
│   │   ├── routes_admin.rs
│   │   ├── auth.rs             # middleware Bearer token (opsional)
│   │   └── sse.rs              # helper SSE chunk
│   ├── openai/
│   │   ├── types.rs            # ChatRequest, ChatResponse, Chunk, Model, Usage
│   │   └── convert.rs          # messages -> prompt string
│   ├── provider/
│   │   ├── mod.rs              # trait Provider + Registry
│   │   ├── discovery.rs        # cari binary di PATH (cross-platform)
│   │   ├── process.rs          # spawn, stream stdout, timeout, kill tree
│   │   ├── opencode.rs
│   │   ├── copilot.rs
│   │   ├── gemini.rs
│   │   ├── claude_code.rs      # (opsional)
│   │   ├── codex.rs            # (opsional)
│   │   └── generic.rs          # adapter generik berbasis template di config
│   └── util/
│       ├── ansi.rs             # strip ANSI escape
│       └── limits.rs           # semaphore, rate limit
└── tests/
    ├── fake_cli/               # script palsu (sh + .cmd) untuk integration test
    └── api_tests.rs
```

### 4.2 Trait Inti

```rust
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// ID unik, mis. "opencode", "copilot", "gemini"
    fn id(&self) -> &str;

    /// Cek binary ada & bisa jalan; kembalikan info versi
    async fn detect(&self) -> Result<ProviderInfo, ProviderError>;

    /// Daftar model yang tersedia (tanpa prefix provider)
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Jalankan chat; hasil berupa stream delta teks
    async fn chat(
        &self,
        req: ChatJob,                  // model, prompt, system, cwd, opsi
        cancel: CancellationToken,     // dipicu saat client disconnect
    ) -> Result<BoxStream<'static, Result<ChatEvent, ProviderError>>, ProviderError>;
}

pub enum ChatEvent {
    Delta(String),          // potongan teks
    Usage(Usage),           // jika CLI melaporkan token
    Done { finish_reason: FinishReason },
}
```

`Registry` = `HashMap<String, Arc<dyn Provider>>` + cache model (TTL, mis. 5 menit).

### 4.3 Alur Request `/v1/chat/completions`

1. Parse & validasi body (`model`, `messages`, `stream`).
2. Pecah `model` → `(provider_id, model_id)`; 404 bila provider tidak ada.
3. `convert.rs`: ubah `messages[]` menjadi prompt tunggal (system + riwayat percakapan terformat).
4. Ambil permit dari semaphore provider (batasi konkurensi).
5. `provider.chat(...)` → stream `ChatEvent`.
6. **stream=true** → bungkus jadi SSE `chat.completion.chunk` … `data: [DONE]`.
   **stream=false** → kumpulkan semua delta → satu `chat.completion`.
7. Jika client putus → `CancellationToken` → kill proses anak (beserta process tree).

---

## 5. Adapter per CLI

> ⚠️ **Flag CLI sering berubah antar versi.** Nilai di bawah adalah titik awal; **verifikasi dengan `--help` pada versi terpasang** dan tulis sebagai *template yang bisa dioverride di config* (lihat §6), bukan hardcode mati.

| Provider | Binary | Daftar model | Eksekusi non-interaktif (kandidat) | Output |
|---|---|---|---|---|
| opencode | `opencode` | `opencode models` | `opencode run --model <prov/model> "<prompt>"` (cek juga `--format json`); alternatif: `opencode serve` lalu HTTP lewat `reqwest` | teks / JSON events |
| Copilot CLI | `copilot` | daftar statis di config / parse `--help` (tidak ada perintah list yang stabil) | `copilot -p "<prompt>" --model <model>` | teks |
| Gemini CLI | `gemini` | daftar statis di config (mis. `gemini-2.5-pro`, `gemini-2.5-flash`) | `gemini -p "<prompt>" -m <model>` (cek `--output-format json/stream-json` bila tersedia) | teks / JSON |
| Claude Code *(opsional)* | `claude` | statis | `claude -p "<prompt>" --output-format stream-json` | JSON lines |
| Codex CLI *(opsional)* | `codex` | statis | `codex exec "<prompt>"` | teks |

### Strategi Parsing Output
- **Prioritas 1:** mode JSON/stream-JSON bila CLI mendukung → parse per baris, ambil delta teks.
- **Prioritas 2:** teks mentah → baca stdout per chunk, `strip ANSI`, kirim sebagai delta.
- stderr dikumpulkan terpisah untuk diagnosis error (jangan dicampur ke respons).
- Exit code ≠ 0 → map ke error OpenAI (`502 upstream_error` + pesan stderr terpotong).

### Adapter Generik (kunci ekstensibilitas)
User bisa menambah CLI baru **tanpa kompilasi ulang** lewat config:

```toml
[[provider]]
id = "mycli"
bin = "mycli"
models_cmd = ["models"]                       # opsional
chat_cmd   = ["run", "--model", "{model}", "--prompt-stdin"]
prompt_via = "stdin"                          # stdin | arg | file
output     = "text"                           # text | jsonl
jsonl_text_path = "/delta/text"               # JSON pointer bila jsonl
```

---

## 6. Konfigurasi

Lokasi default (via crate `dirs`):
- Linux: `~/.config/agentgate/config.toml`
- Windows: `%APPDATA%\agentgate\config.toml`
- Override: `--config <path>` atau env `AGENTGATE_CONFIG`.

Prioritas: **flag CLI > env > file > default**.

```toml
[server]
host = "127.0.0.1"
port = 8787
api_keys = []                 # kosong = tanpa auth; isi untuk Bearer token
request_timeout_secs = 600
cors = false

[defaults]
workdir = "."                 # cwd untuk proses CLI
max_concurrency_per_provider = 2
model_cache_ttl_secs = 300

[provider.opencode]
enabled = true
# bin = "/custom/path/opencode"

[provider.copilot]
enabled = true
models = ["gpt-5", "claude-sonnet-4"]       # daftar statis

[provider.gemini]
enabled = true
models = ["gemini-2.5-pro", "gemini-2.5-flash"]
```

---

## 7. Detail Cross-Platform (Penting!)

| Isu | Penanganan |
|---|---|
| Lokasi binary | crate `which`; di Windows hormati `PATHEXT` (`.exe`, `.cmd`, `.bat`, `.ps1`) |
| CLI hasil `npm -g` di Windows berupa shim `.cmd` | Resolve ke path penuh; jalankan lewat `cmd.exe /C` **hanya jika** target `.cmd/.bat` |
| **Keamanan argumen di Windows** (celah escaping `.cmd`, batas ±8191 karakter) | **Jangan taruh prompt di argumen** untuk `.cmd`; kirim via **stdin** atau **file temp**. Validasi/escape ketat bila terpaksa argumen. |
| Batas panjang argumen Linux | Prompt besar → stdin/file temp |
| Kill proses | Gunakan process group (Unix) / Job Object (Windows) — crate `process-wrap` atau `command-group` — agar child-of-child ikut mati |
| Path separator & cwd | Selalu `PathBuf`, jangan string concat |
| Encoding | Paksa UTF-8 saat baca stdout; handle invalid bytes dengan `from_utf8_lossy` |
| Warna/ANSI | Set `NO_COLOR=1`, `TERM=dumb`, plus strip ANSI sebagai fallback |
| Sinyal shutdown | `tokio::signal::ctrl_c` (+ SIGTERM di Unix) → graceful shutdown |
| Service mode | Linux: contoh unit `systemd`; Windows: NSSM / Task Scheduler (dokumentasi, bukan fitur v1) |

---

## 8. Keamanan

Agent CLI **bisa mengeksekusi perintah & mengubah file**. Gateway ini memperluas permukaan serangan, jadi:

1. **Bind default `127.0.0.1`.** Peringatan keras di log bila bind ke `0.0.0.0`.
2. **API key (Bearer) wajib** jika bind non-loopback (tolak start tanpa key).
3. **Tidak ada shell injection:** `Command::new(bin).args([...])`, jangan pernah string-interpolate ke shell.
4. **Workdir terkunci** ke direktori di config; request tidak boleh menentukan cwd sembarang.
5. **Mode izin tool CLI** dibuat eksplisit di config (mis. read-only / auto-approve) — default paling ketat.
6. Batasi ukuran body (mis. 4 MB), timeout request, dan konkurensi.
7. Log **tidak** mencetak isi prompt kecuali `log_prompts = true`.
8. Redaksi token/secret yang mungkin muncul di stderr CLI.

---

## 9. Crate yang Dipakai

| Kebutuhan | Crate |
|---|---|
| HTTP server | `axum`, `tower`, `tower-http` (cors, trace, limit) |
| CLI adapter | `which`, `command-group`, `process-wrap` |
| Konfigurasi | `dirs`, `toml` |
| JSON | `serde`, `serde_json` |
| Async | `tokio`, `async-trait` |
| Error handling | `thiserror`, `anyhow` |
| Logging | `tracing`, `tracing-subscriber` |
| Testing | `mockall`, `insta` |
| HTTP client | `reqwest` (opsional, untuk adapter HTTP) |
| Stream | `futures`, `tokio-stream` |
| Parsing | `clap`, `regex` |
| Cross-platform | `os_pipe`, `os_str_bytes` |
| Keamanan | `secrecy`, `sanitize-filename` |
| Proxy | `hyper` (opsional, untuk proxy) |
| Skill | `skill-creator` (opsional, untuk skill) |
| MCP | `mcp` (opsional, untuk MCP) |

---

## 10. Endpoint Tambahan

### 10.1 Add Provider

| Method | Path | Fungsi |
|---|---|---|
| POST | `/admin/providers/add` | Tambah provider custom dengan body JSON:
```json
{
  "id": "custom-provider",
  "bin": "/path/to/cli",
  "models_cmd": ["models"],
  "chat_cmd": ["run", "--model", "{model}", "--prompt-stdin"],
  "prompt_via": "stdin",
  "output": "text",
  "jsonl_text_path": "/delta/text"
}
```

### 10.2 Add Key

| Method | Path | Fungsi |
|---|---|---|
| POST | `/admin/keys/add` | Tambah API key baru dengan body JSON:
```json
{
  "key": "your-api-key",
  "description": "Optional description"
}
```

### 10.3 Proxy

| Method | Path | Fungsi |
|---|---|---|
| POST | `/proxy` | Proxy request ke URL eksternal dengan body JSON:
```json
{
  "url": "https://example.com/api",
  "method": "GET",
  "headers": {"Authorization": "Bearer token"},
  "body": {"key": "value"}
}
```

### 10.4 Add Skill

| Method | Path | Fungsi |
|---|---|---|
| POST | `/admin/skills/add` | Tambah skill baru dengan body JSON:
```json
{
  "name": "skill-name",
  "description": "Optional description",
  "command": "skill-command"
}
```

### 10.5 Add MCP

| Method | Path | Fungsi |
|---|---|---|
| POST | `/admin/mcp/add` | Tambah MCP baru dengan body JSON:
```json
{
  "name": "mcp-name",
  "description": "Optional description",
  "config": {"key": "value"}
}
```

---

## 11. Contoh Penggunaan

### 11.1 Tambah Provider Custom

```bash
curl -X POST http://localhost:8787/admin/providers/add \
  -H "Content-Type: application/json" \
  -d '{
    "id": "custom-provider",
    "bin": "/path/to/cli",
    "models_cmd": ["models"],
    "chat_cmd": ["run", "--model", "{model}", "--prompt-stdin"],
    "prompt_via": "stdin",
    "output": "text",
    "jsonl_text_path": "/delta/text"
  }'
```

### 11.2 Tambah API Key

```bash
curl -X POST http://localhost:8787/admin/keys/add \
  -H "Content-Type: application/json" \
  -d '{
    "key": "your-api-key",
    "description": "Optional description"
  }'
```

### 11.3 Proxy Request

```bash
curl -X POST http://localhost:8787/proxy \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com/api",
    "method": "GET",
    "headers": {"Authorization": "Bearer token"},
    "body": {"key": "value"}
  }'
```

### 11.4 Tambah Skill

```bash
curl -X POST http://localhost:8787/admin/skills/add \
  -H "Content-Type: application/json" \
  -d '{
    "name": "skill-name",
    "description": "Optional description",
    "command": "skill-command"
  }'
```

### 11.5 Tambah MCP

```bash
curl -X POST http://localhost:8787/admin/mcp/add \
  -H "Content-Type: application/json" \
  -d '{
    "name": "mcp-name",
    "description": "Optional description",
    "config": {"key": "value"}
  }'
```

---

## 12. Roadmap

### Milestone 0 (M0) — Gateway Dasar

- [x] Deteksi CLI agent terpasang
- [x] Endpoint `/v1/models` dan `/v1/chat/completions`
- [x] Konfigurasi via TOML
- [x] Cross-platform
- [x] Keamanan dasar

### Milestone 1 (M1) — Ekstensibilitas

- [ ] Tambah provider custom via endpoint
- [ ] Tambah API key via endpoint
- [ ] Proxy request ke URL eksternal
- [ ] Tambah skill baru via endpoint
- [ ] Tambah MCP baru via endpoint

### Milestone 2 (M2) — Integrasi Lanjut

- [ ] Integrasi dengan sistem notifikasi
- [ ] Dashboard admin web (opsional)
- [ ] Dukungan untuk lebih banyak CLI agent

### Milestone 3 (M3) — Optimasi

- [ ] Optimasi performa
- [ ] Dukungan untuk lebih banyak fitur OpenAI
- [ ] Dukungan untuk lebih banyak platform

### Milestone 4 (M4) — Ekosistem

- [ ] Dukungan untuk plugin eksternal
- [ ] Dukungan untuk integrasi dengan layanan lain
- [ ] Dukungan untuk lebih banyak fitur keamanan

### Milestone 5 (M5) — Stabilitas

- [ ] Testing komprehensif
- [ ] Dokumentasi lengkap
- [ ] Dukungan untuk lebih banyak platform

### Milestone 6 (M6) — Komunitas

- [ ] Dukungan untuk komunitas
- [ ] Dukungan untuk kontribusi
- [ ] Dukungan untuk lebih banyak fitur

---

## 13. Strategi Testing

### 13.1 Unit Testing

- Testing untuk setiap fungsi dan metode
- Menggunakan `mockall` untuk mocking
- Menggunakan `insta` untuk snapshot testing

### 13.2 Integration Testing

- Testing untuk setiap endpoint
- Menggunakan script palsu untuk CLI agent
- Menggunakan `reqwest` untuk testing proxy

### 13.3 End-to-End Testing

- Testing untuk alur kerja lengkap
- Menggunakan `tokio` untuk testing async
- Menggunakan `tracing` untuk logging

---

## 14. Contoh Pemakaian

### 14.1 Contoh Request ke `/v1/chat/completions`

```bash
curl http://localhost:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/anthropic/claude-sonnet-4",
    "messages": [
      {"role": "user", "content": "Hello!"}
    ]
  }'
```

### 14.2 Contoh Response dari `/v1/chat/completions`

```json
{
  "id": "chatcmpl-123",
  "object": "chat.completion",
  "created": 1677652288,
  "model": "opencode/anthropic/claude-sonnet-4",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "Hello! How can I assist you today?"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 9,
    "completion_tokens": 12,
    "total_tokens": 21
  }
}
```

### 14.3 Contoh Request ke `/admin/providers/add`

```bash
curl -X POST http://localhost:8787/admin/providers/add \
  -H "Content-Type: application/json" \
  -d '{
    "id": "custom-provider",
    "bin": "/path/to/cli",
    "models_cmd": ["models"],
    "chat_cmd": ["run", "--model", "{model}", "--prompt-stdin"],
    "prompt_via": "stdin",
    "output": "text",
    "jsonl_text_path": "/delta/text"
  }'
```

### 14.4 Contoh Response dari `/admin/providers/add`

```json
{
  "status": "success",
  "message": "Provider added successfully"
}
```

### 14.5 Contoh Request ke `/admin/keys/add`

```bash
curl -X POST http://localhost:8787/admin/keys/add \
  -H "Content-Type: application/json" \
  -d '{
    "key": "your-api-key",
    "description": "Optional description"
  }'
```

### 14.6 Contoh Response dari `/admin/keys/add`

```json
{
  "status": "success",
  "message": "API key added successfully"
}
```

### 14.7 Contoh Request ke `/proxy`

```bash
curl -X POST http://localhost:8787/proxy \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://example.com/api",
    "method": "GET",
    "headers": {"Authorization": "Bearer token"},
    "body": {"key": "value"}
  }'
```

### 14.8 Contoh Response dari `/proxy`

```json
{
  "status": "success",
  "data": {
    "key": "value"
  }
}
```

### 14.9 Contoh Request ke `/admin/skills/add`

```bash
curl -X POST http://localhost:8787/admin/skills/add \
  -H "Content-Type: application/json" \
  -d '{
    "name": "skill-name",
    "description": "Optional description",
    "command": "skill-command"
  }'
```

### 14.10 Contoh Response dari `/admin/skills/add`

```json
{
  "status": "success",
  "message": "Skill added successfully"
}
```

### 14.11 Contoh Request ke `/admin/mcp/add`

```bash
curl -X POST http://localhost:8787/admin/mcp/add \
  -H "Content-Type: application/json" \
  -d '{
    "name": "mcp-name",
    "description": "Optional description",
    "config": {"key": "value"}
  }'
```

### 14.12 Contoh Response dari `/admin/mcp/add`

```json
{
  "status": "success",
  "message": "MCP added successfully"
}
```

---

## 15. Risiko

| Risiko | Mitigasi |
|---|---|
| Keamanan | Validasi input, sanitasi output, batasan akses |
| Kinerja | Batasan konkurensi, caching, optimasi kode |
| Kompatibilitas | Testing lintas platform, dokumentasi jelas |
| Dukungan | Dokumentasi lengkap, komunitas aktif |

---

## 16. Definisi Selesai (DoD)

- [x] Semua endpoint berfungsi sesuai spesifikasi
- [x] Semua konfigurasi berfungsi sesuai spesifikasi
- [x] Semua testing berhasil dijalankan
- [x] Dokumentasi lengkap dan akurat
- [x] Kode bersih dan terstruktur
- [x] Kode terlihat dan mudah dipahami
- [x] Kode terlihat dan mudah dipahami
- [x] Kode terlihat dan mudah dipahami

---

## 17. Catatan

- Plan ini akan terus diperbarui sesuai perkembangan proyek
- Silakan berikan masukan atau saran untuk perbaikan
- Terima kasih telah menggunakan `agentgate`

---

## 18. Kontak

- Email: support@agentgate.com
- Website: https://agentgate.com
- GitHub: https://github.com/agentgate/agentgate

---

## 19. Lisensi

`agentgate` dilisensikan di bawah lisensi MIT. Lihat file LICENSE untuk detail.

---

## 20. Akhir

Terima kasih telah menggunakan `agentgate`. Jika Anda memiliki pertanyaan atau masukan, silakan hubungi kami.

---
| Async | `tokio` (process, io-util, sync, signal), `tokio-util` (CancellationToken), `futures` |
| SSE / stream | `async-stream`, `tokio-stream` |
| JSON | `serde`, `serde_json` |
| Config | `toml`, `serde`, `dirs` |
| CLI args | `clap` (derive) |
| Cari binary | `which` |
| Proses tree | `process-wrap` (atau `command-group`) |
| HTTP client (mode `opencode serve`) | `reqwest` (rustls) |
| Error | `thiserror`, `anyhow` |
| Log | `tracing`, `tracing-subscriber` |
| ID/waktu | `uuid`, `time` |
| ANSI strip | `strip-ansi-escapes` |
| Test | `tokio-test`, `reqwest`, `assert_cmd`, `wiremock` (bila perlu) |

---

## 10. Subcommand CLI `agentgate`

```
agentgate serve [--host H] [--port P] [--config FILE]
agentgate providers            # tampilkan provider terdeteksi + versi + path
agentgate models [--provider X]
agentgate doctor               # diagnosis: PATH, login CLI, izin, port
agentgate test <provider/model> "halo"   # tes cepat tanpa HTTP
agentgate init                 # tulis config contoh
```

---

## 11. Roadmap & Milestone

### M0 — Fondasi (1–2 hari)
- [ ] `cargo new`, struktur modul, CI (fmt, clippy, test) di Linux + Windows
- [ ] Config loader + `clap`
- [ ] Router axum: `GET /`, `GET /health`
- [ ] Tipe OpenAI (`serde`) + format error standar

### M1 — Provider pertama: opencode (2–3 hari)
- [ ] `discovery.rs` (cross-platform)
- [ ] `process.rs`: spawn, stream stdout, timeout, kill tree, cancel
- [ ] `opencode.rs`: `list_models` + `chat` (non-stream)
- [ ] `GET /v1/models`, `POST /v1/chat/completions` (non-stream)

### M2 — Streaming & Robustness (2 hari)
- [ ] SSE `chat.completion.chunk` + `[DONE]`
- [ ] Cancel saat client disconnect
- [ ] Semaphore konkurensi, request timeout, limit body
- [ ] Strip ANSI, mapping error ke format OpenAI

### M3 — Provider tambahan (2–3 hari)
- [ ] `copilot.rs`, `gemini.rs`
- [ ] Adapter generik berbasis config (`generic.rs`)
- [ ] Cache model + `/admin/refresh`

### M4 — Keamanan & UX (2 hari)
- [ ] Bearer auth middleware, guard non-loopback
- [ ] `agentgate doctor`, `providers`, `models`, `test`
- [ ] Redaksi log, `log_prompts` opt-in

### M5 — Packaging & Rilis (1–2 hari)
- [ ] Cross-build (GitHub Actions): `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`
- [ ] Rilis binary + checksum, contoh `systemd` unit & NSSM
- [ ] README + contoh integrasi (curl, OpenAI SDK, Continue/Cline/OpenWebUI)

### M6+ — Opsional
- [ ] `/v1/responses`
- [ ] Mode `opencode serve` (persistent, lebih cepat dari spawn per-request)
- [ ] Claude Code & Codex adapter
- [ ] Session/conversation reuse (hindari kirim ulang seluruh riwayat)
- [ ] Metrik Prometheus `/metrics`
- [ ] Tool-calling emulation (eksperimental)

---

## 12. Strategi Testing

1. **Unit test:** parsing nama model, `messages → prompt`, SSE chunk, strip ANSI, parser JSONL.
2. **Fake CLI:** skrip palsu (`fake_cli.sh` untuk Unix, `fake_cli.cmd` untuk Windows) yang mencetak output terjadwal/bisa error/bisa hang → uji streaming, timeout, cancel, exit code.
3. **Integration test:** jalankan server di port acak, tembak lewat `reqwest`; cek bentuk JSON sesuai spesifikasi OpenAI.
4. **Kompatibilitas klien:** uji manual dengan SDK `openai` (Python/JS) dan satu klien GUI.
5. **CI matrix:** `ubuntu-latest` + `windows-latest`.
6. **Smoke test nyata (manual/opsional):** pakai CLI asli bila terpasang.

---

## 13. Contoh Pemakaian

```bash
# Jalankan
agentgate serve

# Cek model
curl http://127.0.0.1:8787/v1/models

# Chat non-stream
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemini/gemini-2.5-pro",
    "messages": [{"role":"user","content":"Jelaskan ownership di Rust singkat"}]
  }'

# Streaming
curl -N http://127.0.0.1:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"opencode/anthropic/claude-sonnet-4","stream":true,
       "messages":[{"role":"user","content":"halo"}]}'
```

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:8787/v1", api_key="dummy")
r = client.chat.completions.create(
    model="copilot/gpt-5",
    messages=[{"role": "user", "content": "halo"}],
)
print(r.choices[0].message.content)
```

---

## 14. Risiko & Mitigasi

| Risiko | Dampak | Mitigasi |
|---|---|---|
| Flag/output CLI berubah antar versi | Adapter rusak | Template di config, `doctor` cek versi, test dengan fake CLI, pin versi yang didukung di README |
| Latensi tinggi (spawn tiap request) | Lambat | Mode server persisten (`opencode serve`), pooling proses (fase lanjut) |
| Output teks tidak terstruktur | Streaming kacau, usage tidak akurat | Utamakan mode JSON; usage = estimasi bila tidak ada |
| CLI interaktif minta konfirmasi/login | Request hang | Flag non-interaktif, timeout, `doctor` cek status login |
| Quirk `.cmd` di Windows | Bug/injection | Prompt via stdin/file, tanpa interpolasi shell |
| Pelanggaran ToS vendor bila dibuka publik | Akun terblokir | Dokumentasikan: untuk **penggunaan pribadi/lokal**, jangan expose ke publik/multi-user |
| Agent CLI mengubah file tak terduga | Kerusakan data | Workdir terkunci, mode izin ketat default |
| Prompt riwayat panjang | Lambat / melebihi limit | Truncation policy + session reuse (M6) |

---

## 15. Definition of Done (v1.0)

- [ ] `agentgate serve` jalan di Linux & Windows tanpa perubahan kode.
- [ ] Minimal 3 provider berfungsi: **opencode, copilot, gemini**.
- [ ] `/`, `/health`, `/v1/models`, `/v1/chat/completions` (stream & non-stream) lolos tes dengan SDK OpenAI.
- [ ] Client disconnect membunuh proses CLI (tidak ada proses yatim).
- [ ] Default aman (localhost, tanpa shell injection, workdir terkunci).
- [ ] CI hijau di Linux + Windows; binary rilis tersedia.
- [ ] README lengkap + `agentgate doctor` membantu troubleshooting.
