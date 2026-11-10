# AgentGate Installation Guide

Cross-platform installation guide for AgentGate — an OpenAI-compatible gateway that routes chat requests to local model CLIs (`opencode`, `gemini`, `claude`) and the Ollama HTTP API.

## Quick Start

```bash
# Linux/macOS (binary release)
curl -sSL https://github.com/mrjay59/agengate/releases/latest/download/agentgate-linux-x86_64.zip -o /tmp/agentgate.zip
unzip -o /tmp/agentgate.zip -d /tmp/agentgate && sudo mv /tmp/agentgate/agentgate /usr/local/bin/
agentgate serve --host 0.0.0.0 --port 8787

# Windows (PowerShell)
Invoke-WebRequest -Uri "https://github.com/mrjay59/agengate/releases/latest/download/agentgate-windows-x86_64.zip" -OutFile "$env:TEMP\agentgate.zip"
Expand-Archive "$env:TEMP\agentgate.zip" -DestinationPath "$env:USERPROFILE\bin" -Force
agentgate.exe serve --host 0.0.0.0 --port 8787

# ARM64 (Linux)
curl -sSL https://github.com/mrjay59/agengate/releases/latest/download/agentgate-linux-aarch64.zip -o /tmp/agentgate.zip
unzip -o /tmp/agentgate.zip -d /tmp/agentgate && sudo mv /tmp/agentgate/agentgate /usr/local/bin/
agentgate serve --host 0.0.0.0 --port 8787
```

> The server only needs to be reachable by your client. Ollama-native mode additionally requires a running Ollama server at `base_url` (default `http://127.0.0.1:11434`).

### First run (required)

After extracting the binary, run once to generate the config file:

```bash
agentgate init          # creates agentgate.toml in the current dir (or ~/.config/agentgate/)
agentgate doctor        # check provider availability
agentgate models        # list available models
agentgate serve         # start the gateway
```

> Without `agentgate init`, no providers are configured and `agentgate models` returns an empty list. See the **Configuration** section for the full `agentgate.toml` reference.

---

## Prerequisites

| Platform | Requirements |
|----------|--------------|
| **Linux** | glibc 2.31+, ca-certificates |
| **Windows** | Windows 10+ |
| **ARM64** | glibc 2.31+ (aarch64/arm64) |
| **All** | Rust 1.75+ (only when building from source) |

### CLI Providers (optional, for CLI-agent mode)

| Provider | Install |
|---|---|
| `opencode` | `npm install -g opencode-ai/opencode` |
| `gemini` | `npm install -g @google/generative-ai-cli` |
| `claude` | `npm install -g @anthropic-ai/claude-code` |

### Ollama (optional, for native HTTP mode)

```bash
# macOS
brew install ollama

# Linux (official script)
curl -fsSL https://ollama.com/install.sh | sh

# Windows
winget install Ollama.Ollama

# Then pull a model
ollama pull qwen2.5:7b
```

No Ollama binary is needed on the AgentGate host for native mode — only the HTTP API.

---

## Linux Installation

### Method 1: Binary Release (Recommended)

```bash
# x86_64
curl -sSL https://github.com/mrjay59/agengate/releases/latest/download/agentgate-linux-x86_64.zip -o /tmp/agentgate.zip
unzip -o /tmp/agentgate.zip -d /tmp/agentgate && sudo mv /tmp/agentgate/agentgate /usr/local/bin/

# ARM64 (Raspberry Pi, ARM servers)
curl -sSL https://github.com/mrjay59/agengate/releases/latest/download/agentgate-linux-aarch64.zip -o /tmp/agentgate.zip
unzip -o /tmp/agentgate.zip -d /tmp/agentgate && sudo mv /tmp/agentgate/agentgate /usr/local/bin/

# Verify
agentgate --version
```

### Method 2: Cargo (From Source)

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Install agentgate
cargo install --git https://github.com/mrjay59/agengate.git

# Or build locally
git clone https://github.com/mrjay59/agengate.git
cd agentgate
cargo build --release
sudo cp target/release/agentgate /usr/local/bin/
```

### Method 3: Package Managers

```bash
# Arch Linux (AUR)
yay -S agentgate-git

# NixOS
nix-env -iA nixos.agentgate

# Homebrew (Linux)
brew install agentgate/tap/agentgate
```

### systemd Service

```bash
sudo tee /etc/systemd/system/agentgate.service > /dev/null <<EOF
[Unit]
Description=AgentGate OpenAI-compatible CLI Gateway
After=network.target

[Service]
Type=simple
User=agentgate
Group=agentgate
WorkingDirectory=/opt/agentgate
ExecStart=/usr/local/bin/agentgate serve --host 0.0.0.0 --port 8787
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now agentgate
```

---

## Windows Installation

### Method 1: Binary Release (Recommended)

```powershell
$url    = "https://github.com/mrjay59/agengate/releases/latest/download/agentgate-windows-x86_64.zip"
$output = "$env:TEMP\agentgate.zip"
Invoke-WebRequest -Uri $url -OutFile $output
Expand-Archive $output -DestinationPath "$env:USERPROFILE\bin" -Force

# Add to PATH (persistent)
[Environment]::SetEnvironmentVariable("Path",
  [Environment]::GetEnvironmentVariable("Path", "User") + ";$env:USERPROFILE\bin", "User")

# Verify
agentgate.exe --version
```

### Method 2: Cargo (From Source)

```powershell
winget install Rustlang.Rust
# restart shell
cargo install --git https://github.com/mrjay59/agengate.git

# Or build locally
git clone https://github.com/mrjay59/agengate.git
cd agentgate
cargo build --release
copy target\release\agentgate.exe $env:USERPROFILE\bin\
$env:Path += ";$env:USERPROFILE\bin"
```

### Method 3: Scoop / Chocolatey

```powershell
scoop bucket add agentgate https://github.com/agentgate/scoop-bucket
scoop install agentgate

# or
choco install agentgate
```

### Windows Service (NSSM)

```powershell
scoop install nssm
nssm install AgentGate "C:\Users\$env:USERNAME\bin\agentgate.exe"
nssm set AgentGate AppParameters "serve --host 0.0.0.0 --port 8787"
nssm set AgentGate AppDirectory "C:\Users\$env:USERNAME\AppData\Roaming\agentgate"
nssm set AgentGate Start SERVICE_AUTO_START
nssm start AgentGate
```

---

## ARM64 / Mobile

### Android (Termux)

```bash
pkg update && pkg upgrade
pkg install rust openssl ca-certificates

git clone https://github.com/mrjay59/agengate.git
cd agentgate
cargo build --release

mkdir -p $HOME/bin
cp target/release/agentgate $HOME/bin/
echo 'export PATH=$PATH:$HOME/bin' >> $HOME/.bashrc
source $HOME/.bashrc

agentgate serve --host 0.0.0.0 --port 8787
```

### Raspberry Pi / ARM SBC

```bash
# Pre-built binary
curl -sSL https://github.com/mrjay59/agengate/releases/latest/download/agentgate-linux-aarch64.zip -o /tmp/agentgate.zip
unzip -o /tmp/agentgate.zip -d /tmp/agentgate && sudo mv /tmp/agentgate/agentgate /usr/local/bin/

# Or build natively
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
git clone https://github.com/mrjay59/agengate.git
cd agentgate
cargo build --release
sudo cp target/release/agentgate /usr/local/bin/
```

### Cross-compile (x86_64 → aarch64)

```bash
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu

cargo build --release --target aarch64-unknown-linux-gnu
scp target/aarch64-unknown-linux-gnu/release/agentgate user@pi:/usr/local/bin/
```

---

## Configuration

Config file locations:

| Platform | Path |
|----------|------|
| Linux/macOS | `~/.config/agentgate/agentgate.toml` or `./agentgate.toml` |
| Windows | `%APPDATA%\agentgate\agentgate.toml` or `.\agentgate.toml` |

Generate a template:

```bash
agentgate init
```

### Sample `agentgate.toml`

```toml
[server]
host = "0.0.0.0"
port = 8787
# api_keys = ["sk-xxx"]          # enable when auth is needed
request_timeout_secs = 600
cors = false

[defaults]
workdir = "."
max_concurrency_per_provider = 2
model_cache_ttl_secs = 300

# CLI-agent providers (spawn a local CLI process per request)

[[provider]]
id = "opencode"
bin = "opencode"
models_cmd = ["models"]
chat_cmd = ["run", "--model", "{model}", "{prompt}"]
strip_ansi = true
enabled = true

[[provider]]
id = "gemini"
bin = "gemini"
models_cmd = ["list", "--format", "json"]
chat_cmd = ["generate", "--model", "{model}", "{prompt}"]
strip_ansi = true
enabled = true
models = ["gemini-2.5-pro", "gemini-2.5-flash"]

[[provider]]
id = "claude"
bin = "claude"
chat_cmd = ["-p", "--output-format", "stream-json", "--verbose",
            "--include-partial-messages", "--model", "{model}"]
strip_ansi = true
enabled = true
models = ["sonnet", "opus", "haiku"]

# Native HTTP provider (Ollama)

[[provider]]
id = "ollama"
bin = "ollama"
enabled = true
base_url = "http://127.0.0.1:11434"
num_ctx = 16384
keep_alive = "10m"

# Reasoning / thinking output

[reasoning]
enabled = true
format = "reasoning_content"     # reasoning_content | reasoning | both | think_tags | none | passthrough
fallback = "activity"            # none | activity | induce
omitted_marker = "activity"      # none | activity
activity_lang = "id"             # id | en
activity_prefix = "⏳ "
null_content = true
max_chars_per_turn = 200000
max_blocks_per_turn = 64
```

See `agentgate.example.toml` in the repo for the full reference.

---

## Running & Testing

```bash
agentgate serve --host 0.0.0.0 --port 8787
```

### Health check

```bash
curl http://localhost:8787/health
# {"status":"ok"}
```

### List models

```bash
curl http://localhost:8787/v1/models
```

### Chat completion

```bash
curl -X POST http://localhost:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "ollama/qwen2.5:7b",
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": false
  }'
```

### Streaming chat

```bash
curl -N -X POST http://localhost:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/anthropic/claude-sonnet-4",
    "messages": [{"role": "user", "content": "Think step by step: 47 × 53"}],
    "stream": true
  }'
```

---

## Troubleshooting

### Provider not detected

```bash
which opencode            # Linux/macOS
where opencode            # Windows
```

Add the binary directory to `PATH` (or `bin`) and restart the gateway.

### Port already in use

```bash
lsof -ti:8787 | xargs kill -9     # Linux/macOS
netstat -ano | findstr :8787      # Windows
```

### Ollama connection refused

Ensure `ollama serve` is running and reachable at `base_url`. Default is `http://127.0.0.1:11434`. Pull at least one model (`ollama pull <name>`).

### Windows `.cmd` shims / quoting

Windows `where` resolves `.cmd`/`.bat` wrappers. Use a quoted `bin` path or a `.ps1` shim if you hit quoting issues with `chat_cmd`.

### CLI not exposing reasoning

Verify the CLI version flags. Claude requires `--include-partial-messages --verbose`. `agentgate doctor --probe-reasoning` prints a small SSE probe you can inspect.

---

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `AGENTGATE_CONFIG` | Config file path | auto-detected |
| `AGENTGATE_HOST` | Server bind address | `0.0.0.0` |
| `AGENTGATE_PORT` | Server port | `8787` |
| `RUST_LOG` | Log level (trace/debug/info/warn/error) | `info` |

---

## Updating

```bash
# Binary (Linux)
curl -sSL https://github.com/mrjay59/agengate/releases/latest/download/agentgate-linux-x86_64.zip -o /tmp/agentgate.zip
unzip -o /tmp/agentgate.zip -d /tmp/agentgate && sudo mv /tmp/agentgate/agentgate /usr/local/bin/

# Cargo
cargo install --git https://github.com/mrjay59/agengate.git --force

# Verify
agentgate --version
```

---

## Uninstalling

```bash
sudo rm /usr/local/bin/agentgate
sudo rm -rf /etc/systemd/system/agentgate.service
sudo systemctl daemon-reload

# or, if installed via cargo
cargo uninstall agentgate

# remove config (optional)
rm -rf ~/.config/agentgate
```

---

*AgentGate v0.1.0*
