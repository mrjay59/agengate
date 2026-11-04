# AgentGate

Jembatan antara AionUi dan CLI agent. Menyediakan endpoint HTTP kompatibel OpenAI (`/v1/chat/completions`, `/v1/models`) yang meneruskan request ke CLI tool lokal.

## Fitur

- Kompatibel OpenAI Chat Completions (streaming & non-streaming).
- Mendukung provider CLI (opencode, aionrs, claude, dll).
- Sanitasi pesan `tool` → `assistant` agar provider ketat tetap menerima.
- Mendukung `reasoning_content` / thinking (OpenCode: `--thinking --format json`) untuk stream & non-stream.
- Cross-platform: Windows, Linux x86_64, Linux ARM64.

## Build

### Prasyarat

- Rust toolchain (stable): https://rustup.rs
- Untuk cross-build: Docker Desktop + `cargo install cross`

### Build native (platform saat ini)

```bash
cargo build --release
```

Binary: `target/release/agentgate` (Linux/macOS) atau `target/release/agentgate.exe` (Windows).

### Build multi-platform

```bash
# Linux x86_64 (static, tanpa glibc dependency)
cross build --release --target x86_64-unknown-linux-musl

# Linux ARM64
cross build --release --target aarch64-unknown-linux-gnu

# Windows x86_64 (dari Linux/Mac)
cross build --release --target x86_64-pc-windows-gnu
```

Output binary ada di `target/<target>/release/`.

### Build semua sekaligus (Windows bat)

```bat
@echo off
cross build --release --target x86_64-pc-windows-gnu
cross build --release --target x86_64-unknown-linux-musl
cross build --release --target aarch64-unknown-linux-gnu
echo Selesai. Cek folder target/
```

## Install

### Windows

1. Copy `agentgate.exe` ke folder yang ada di PATH, misal `C:\tools\agentgate\`.
2. Atau jalankan langsung dari folder project:
   ```powershell
   .\target\release\agentgate.exe serve
   ```

### Linux (x86_64 / ARM64)

```bash
chmod +x target/release/agentgate
sudo cp target/release/agentgate /usr/local/bin/agentgate
agentgate serve
```

Untuk ARM (Raspberry Pi, dll), gunakan binary hasil build `aarch64-unknown-linux-gnu`.

### macOS

```bash
cargo build --release
cp target/release/agentgate /usr/local/bin/agentgate
agentgate serve
```

## Konfigurasi

Buat config default:

```bash
agentgate init
```

File config default berada di lokasi standar OS. Edit `config.toml` untuk menambahkan provider CLI:

```toml
[server]
host = "127.0.0.1"
port = 8080

[[provider]]
id = "opencode"
bin = "opencode"
models = ["opencode/beam-free"]
chat_cmd = ["run", "--model", "{model}", "--thinking", "--format", "json", "{prompt}"]
strip_ansi = true

[[provider]]
id = "ollama"
bin = "ollama"
models_cmd = ["list"]
chat_cmd = ["run", "{model}"]
strip_ansi = true
enabled = true
models = ["llama3", "deepseek-r1", "phi3"]
```

Parameter penting:
- `bin`: path/nama binary CLI.
- `models_cmd`: perintah untuk list model (opsional, fallback ke `models`).
- `chat_cmd`: perintah chat, gunakan placeholder `{model}` dan `{prompt}`.
- `strip_ansi`: hapus ANSI escape codes dari output.

## Menjalankan

```bash
agentgate serve
# atau dengan host/port custom
agentgate serve --host 0.0.0.0 --port 9000
```

## Penggunaan dengan AionUi

Di AionUi, tambahkan Custom API provider dengan:
- Base URL: `http://127.0.0.1:8080/v1`
- API Key: (bebas, AgentGate tidak validasi)
- Model: sesuai yang terdaftar (misal `opencode/beam-free`)

## Endpoint

| Method | Path | Deskripsi |
|--------|------|-----------|
| GET | `/v1/models` | List model tersedia |
| POST | `/v1/chat/completions` | Chat completion (stream/non-stream) |
| GET | `/health` | Health check |

## Troubleshooting

- **`UNKNOWN_UPSTREAM_ERROR` / `OpenAI stream ended without a terminal event`**: Pastikan AgentGate versi terbaru (sudah wrap SSE + `[DONE]`).
- **Error 422 `unknown variant tool`**: Versi terbaru sudah map `tool` → `assistant`.
- **Error 422 `missing field content`**: Versi terbaru sudah pakai `#[serde(default)]`.
- **Output kosong**: Cek `strip_ansi`, cek path `bin`, cek `chat_cmd`.

## Lisensi

MIT