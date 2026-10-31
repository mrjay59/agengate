# AgentGate Installation Guide

Cross-platform installation guide for AgentGate - OpenAI-compatible CLI gateway.

## Quick Start

```bash
# Linux/macOS
curl -sSL https://github.com/agentgate/agentgate/releases/latest/download/agentgate-linux-x86_64.tar.gz | tar xz
sudo mv agentgate /usr/local/bin/
agentgate serve --host 0.0.0.0 --port 8787

# Windows (PowerShell)
Invoke-WebRequest -Uri "https://github.com/agentgate/agentgate/releases/latest/download/agentgate-windows-x86_64.zip" -OutFile "agentgate.zip"
Expand-Archive agentgate.zip
./agentgate.exe serve --host 0.0.0.0 --port 8787

# ARM64 (mobile/embedded)
curl -sSL https://github.com/agentgate/agentgate/releases/latest/download/agentgate-linux-aarch64.tar.gz | tar xz
sudo mv agentgate /usr/local/bin/
agentgate serve --host 0.0.0.0 --port 8787
```

---

## Prerequisites

| Platform | Requirements |
|----------|--------------|
| **Linux** | glibc 2.31+, openssl, ca-certificates |
| **Windows** | Windows 10+, Visual C++ Redistributable |
| **ARM64** | glibc 2.31+, openssl (aarch64/arm64) |
| **All** | Rust 1.75+ (for building from source) |

### CLI Providers (Optional but Required for Functionality)

```bash
# opencode (recommended)
npm install -g @opencode/opencode

# GitHub Copilot CLI
npm install -g @githubnext/github-copilot-cli

# Google Gemini CLI
npm install -g @google/generative-ai-cli
```

---

## Linux Installation

### Method 1: Binary Release (Recommended)

```bash
# x86_64
curl -sSL https://github.com/agentgate/agentgate/releases/latest/download/agentgate-linux-x86_64.tar.gz \
  | sudo tar xz -C /usr/local/bin

# ARM64 (Raspberry Pi, ARM servers)
curl -sSL https://github.com/agentgate/agentgate/releases/latest/download/agentgate-linux-aarch64.tar.gz \
  | sudo tar xz -C /usr/local/bin

# Verify
agentgate --version
```

### Method 2: Cargo (From Source)

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Install agentgate
cargo install --git https://github.com/agentgate/agentgate.git

# Or build locally
git clone https://github.com/agentgate/agentgate.git
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

### systemd Service (Auto-start)

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
# Download and extract
$url = "https://github.com/agentgate/agentgate/releases/latest/download/agentgate-windows-x86_64.zip"
$output = "$env:TEMP\agentgate.zip"
Invoke-WebRequest -Uri $url -OutFile $output
Expand-Archive $output -DestinationPath "$env:USERPROFILE\bin" -Force

# Add to PATH (persistent)
[Environment]::SetEnvironmentVariable("Path", [Environment]::GetEnvironmentVariable("Path", "User") + ";$env:USERPROFILE\bin", "User")

# Verify
agentgate.exe --version
```

### Method 2: Cargo (From Source)

```powershell
# Install Rust
winget install Rustlang.Rust

# Restart PowerShell, then:
cargo install --git https://github.com/agentgate/agentgate.git

# Or build locally
git clone https://github.com/agentgate/agentgate.git
cd agentgate
cargo build --release
copy target\release\agentgate.exe $env:USERPROFILE\bin\

# Add to PATH if needed
$env:Path += ";$env:USERPROFILE\bin"
```

### Method 3: Scoop / Chocolatey

```powershell
# Scoop
scoop bucket add agentgate https://github.com/agentgate/scoop-bucket
scoop install agentgate

# Chocolatey
choco install agentgate
```

### Windows Service (NSSM)

```powershell
# Install NSSM
scoop install nssm

# Create service
nssm install AgentGate "C:\Users\$env:USERNAME\bin\agentgate.exe"
nssm set AgentGate AppParameters "serve --host 0.0.0.0 --port 8787"
nssm set AgentGate AppDirectory "C:\Users\$env:USERNAME\AppData\Roaming\agentgate"
nssm set AgentGate Start SERVICE_AUTO_START
nssm start AgentGate
```

---

## ARM64 / Mobile Installation

### Android (Termux)

```bash
# Install dependencies
pkg update && pkg upgrade
pkg install rust openssl ca-certificates

# Build (takes 10-30 min on mobile)
git clone https://github.com/agentgate/agentgate.git
cd agentgate
cargo build --release

# Install
mkdir -p $HOME/bin
cp target/release/agentgate $HOME/bin/
echo 'export PATH=$PATH:$HOME/bin' >> $HOME/.bashrc
source $HOME/.bashrc

# Run
agentgate serve --host 0.0.0.0 --port 8787
```

### iOS (ish / a-Shell)

```bash
# ish (Alpine-based)
apk add rust cargo openssl-dev
git clone https://github.com/agentgate/agentgate.git
cd agentgate
cargo build --release
cp target/release/agentgate /usr/local/bin/

# a-Shell (iOS)
# Limited Rust support - use pre-built binary if available
```

### Raspberry Pi / ARM SBC

```bash
# Raspberry Pi OS (64-bit)
curl -sSL https://github.com/agentgate/agentgate/releases/latest/download/agentgate-linux-aarch64.tar.gz \
  | sudo tar xz -C /usr/local/bin

# Or build natively (faster on Pi 4/5)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
git clone https://github.com/agentgate/agentgate.git
cd agentgate
cargo build --release
sudo cp target/release/agentgate /usr/local/bin/
```

### Cross-compilation (x86_64 → ARM64)

```bash
# On x86_64 host
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu

# Build
cargo build --release --target aarch64-unknown-linux-gnu

# Copy target/release/agentgate to ARM device
scp target/aarch64-unknown-linux-gnu/release/agentgate user@pi:/usr/local/bin/
```

---

## Configuration

### Config File Location

| Platform | Path |
|----------|------|
| Linux/macOS | `~/.config/agentgate/agentgate.toml` or `./agentgate.toml` |
| Windows | `%APPDATA%\agentgate\agentgate.toml` or `.\agentgate.toml` |

### Sample Configuration (`agentgate.toml`)

```toml
[server]
host = "0.0.0.0"
port = 8787
api_keys = []  # Add API keys for auth: ["sk-xxx", "sk-yyy"]
request_timeout_secs = 600
cors = false

[defaults]
workdir = "."
max_concurrency_per_provider = 2
model_cache_ttl_secs = 300

[[provider]]
id = "opencode"
bin = "opencode"
models_cmd = ["models"]
chat_cmd = ["run", "--model", "{model}", "{prompt}"]
strip_ansi = true
enabled = true

[[provider]]
id = "copilot"
bin = "gh"
models_cmd = ["api", "v1/copilot/models", "--jq", ".data[].name"]
chat_cmd = ["api", "v1/copilot/chat", "--method", "POST", "--input", "-"]
strip_ansi = true
enabled = true
models = ["gpt-4o", "claude-3-5-sonnet"]

[[provider]]
id = "gemini"
bin = "gemini"
models_cmd = ["list", "--format", "json"]
chat_cmd = ["generate", "--model", "{model}", "{prompt}"]
strip_ansi = true
enabled = true
models = ["gemini-2.5-pro", "gemini-2.5-flash"]
```

### Generate Config Template

```bash
agentgate init  # Creates agentgate.toml in current directory
```

---

## Testing Installation

### Health Check

```bash
curl http://localhost:8787/health
# Response: {"status":"ok"}
```

### List Models

```bash
curl http://localhost:8787/v1/models
# Response: {"object":"list","data":[{"id":"model1",...},...]}
```

### Chat Completion

```bash
curl -X POST http://localhost:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "9router/free-forever",
    "messages": [{"role": "user", "content": "Hello, who are you?"}],
    "stream": false
  }'
```

### With Specific Provider

```bash
curl -X POST http://localhost:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "9router/free-forever",
    "provider": "opencode",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

---

## Troubleshooting

### Provider Not Detected

```bash
# Check if binary in PATH
which opencode          # Linux/macOS
where opencode          # Windows
$env:Path -split ";"    # PowerShell

# Add to PATH if missing
export PATH=$PATH:$HOME/.npm-global/bin  # Linux/macOS
$env:Path += ";$env:APPDATA\npm"         # Windows
```

### Port Already in Use

```bash
# Find and kill process
lsof -ti:8787 | xargs kill -9    # Linux/macOS
netstat -ano | findstr :8787     # Windows
Stop-Process -Id <PID> -Force    # PowerShell
```

### Permission Denied (Linux)

```bash
# Fix binary permissions
chmod +x /usr/local/bin/agentgate

# Or run with sudo (not recommended for production)
sudo agentgate serve --host 0.0.0.0 --port 8787
```

### SSL/TLS Certificates

```bash
# Linux: Install ca-certificates
sudo apt install ca-certificates  # Debian/Ubuntu
sudo dnf install ca-certificates  # Fedora/RHEL

# Alpine: update-ca-certificates
```

---

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `AGENTGATE_CONFIG` | Config file path | Auto-detected |
| `RUST_LOG` | Log level (trace, debug, info, warn, error) | `info` |
| `AGENTGATE_HOST` | Server bind address | `0.0.0.0` |
| `AGENTGATE_PORT` | Server port | `8787` |

Example:
```bash
RUST_LOG=debug AGENTGATE_PORT=8080 agentgate serve
```

---

## Updating

```bash
# Binary
curl -sSL https://github.com/agentgate/agentgate/releases/latest/download/agentgate-linux-x86_64.tar.gz \
  | sudo tar xz -C /usr/local/bin

# Cargo
cargo install --git https://github.com/agentgate/agentgate.git --force

# Verify
agentgate --version
```

---

## Uninstalling

```bash
# Binary
sudo rm /usr/local/bin/agentgate
sudo rm -rf /etc/systemd/system/agentgate.service
sudo systemctl daemon-reload

# Cargo
cargo uninstall agentgate

# Config (optional)
rm -rf ~/.config/agentgate
```

---

## Support

- **Issues**: [GitHub Issues](https://github.com/agentgate/agentgate/issues)
- **Discussions**: [GitHub Discussions](https://github.com/agentgate/agentgate/discussions)
- **Documentation**: [docs.agentgate.dev](https://docs.agentgate.dev)

---

*Generated for AgentGate v0.1.0*