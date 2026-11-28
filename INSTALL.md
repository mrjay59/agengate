# AgentGate Installation Guide

Simple install and deployment guide for **AgentGate v0.3.43** on Windows, Linux, macOS, Docker, and VPS.

AgentGate is an OpenAI-compatible gateway. It listens on HTTP (default `127.0.0.1:8787`) and routes chat requests to configured CLI/HTTP providers.

## Before you install

| Item | Recommendation |
|---|---|
| Config file | Start from `agentgate.example.toml` and save as `agentgate.toml`. |
| Local-only use | Keep `server.host = "127.0.0.1"`. |
| Network/VPS use | Set `server.api_keys = ["..."]` before binding to `0.0.0.0` or exposing publicly. |
| Secrets | Use environment variables for provider keys (e.g. `NVIDIA_API_KEY`). Do not commit secrets. |
| CLI providers | Install the provider CLI you enable (`opencode`, `gemini`, etc.) and make sure it is on `PATH`. |

## Quick install matrix

| Platform | Recommended install | Run as service |
|---|---|---|
| Windows | Release `.exe` or `cargo build --release` | Manual / Task Scheduler / NSSM (optional) |
| Linux | Release binary or build from source | `systemd` |
| macOS | Release binary or build from source | `launchd` or manual |
| Docker | `docker build` / container run | Container runtime restart policy |
| VPS | Linux binary + systemd | systemd + firewall/reverse proxy |

---

## 1) Windows (native)

### Option A: Release binary

1. Download the Windows release from [GitHub Releases](https://github.com/mrjay59/agengate/releases).
2. Extract `agentgate.exe` to a folder, e.g. `C:\Tools\agentgate\`.
3. Copy config:

```powershell
cd C:\Tools\agentgate
Copy-Item agentgate.example.toml agentgate.toml
notepad agentgate.toml
```

4. Set at least:

```toml
[server]
host = "127.0.0.1"
port = 8787
api_keys = ["change-me"]
```

5. Run:

```powershell
.\agentgate.exe serve --config .\agentgate.toml
```

### Option B: Build from source

```powershell
git clone https://github.com/mrjay59/agengate.git
cd agentgate
cargo build --release
Copy-Item agentgate.example.toml agentgate.toml
.\target\release\agentgate.exe serve --config .\agentgate.toml
```

### Windows notes

- If a CLI provider is used, ensure its binary is in `PATH` for the same user/session running AgentGate.
- For local testing, keep `127.0.0.1`. Do not bind `0.0.0.0` without `api_keys`.
- Logs go to stdout/stderr; run in a terminal first to confirm startup.

---

## 2) Linux (generic + systemd)

### Install binary

```bash
# Example: build from source
git clone https://github.com/mrjay59/agengate.git
cd agentgate
cargo build --release
sudo install -m 0755 target/release/agentgate /usr/local/bin/agentgate
```

Or download the Linux release asset from GitHub Releases and place it in `/usr/local/bin/agentgate`.

### Configure

```bash
sudo mkdir -p /etc/agentgate /var/lib/agentgate
sudo cp agentgate.example.toml /etc/agentgate/agentgate.toml
sudo nano /etc/agentgate/agentgate.toml
```

Recommended minimum:

```toml
[server]
host = "127.0.0.1"      # use 0.0.0.0 only with api_keys and firewall
port = 8787
api_keys = ["replace-with-strong-key"]

[defaults]
workdir = "/var/lib/agentgate"
```

### systemd service

Create `/etc/systemd/system/agentgate.service`:

```ini
[Unit]
Description=AgentGate OpenAI-compatible AI gateway
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=agentgate
Group=agentgate
WorkingDirectory=/var/lib/agentgate
ExecStart=/usr/local/bin/agentgate serve --config /etc/agentgate/agentgate.toml
Restart=on-failure
RestartSec=5

# Process/resource safety
KillMode=control-group
TasksMax=1024
MemoryMax=8G

# Optional hardening
NoNewPrivileges=true
PrivateTmp=true

Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

Create user and start:

```bash
sudo useradd --system --home /var/lib/agentgate --shell /usr/sbin/nologin agentgate || true
sudo chown -R agentgate:agentgate /var/lib/agentgate /etc/agentgate
sudo systemctl daemon-reload
sudo systemctl enable --now agentgate
sudo systemctl status agentgate --no-pager
```

Check logs:

```bash
journalctl -u agentgate -f
```

> Adjust `User`, paths, `MemoryMax`, and `TasksMax` to your machine. The values above are safe starting points for VPS/small servers.

---

## 3) macOS (binary + launchd/manual)

### Install

```bash
git clone https://github.com/mrjay59/agengate.git
cd agentgate
cargo build --release
sudo install -m 0755 target/release/agentgate /usr/local/bin/agentgate
mkdir -p ~/.config/agentgate
cp agentgate.example.toml ~/.config/agentgate/agentgate.toml
```

Edit config:

```bash
nano ~/.config/agentgate/agentgate.toml
```

### Run manually

```bash
agentgate serve --config ~/.config/agentgate/agentgate.toml
```

### launchd (user agent)

Create `~/Library/LaunchAgents/com.agentgate.server.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>com.agentgate.server</string>
    <key>ProgramArguments</key>
    <array>
      <string>/usr/local/bin/agentgate</string>
      <string>serve</string>
      <string>--config</string>
      <string>/Users/YOUR_USER/.config/agentgate/agentgate.toml</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/agentgate.out.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/agentgate.err.log</string>
  </dict>
</plist>
```

Load it:

```bash
launchctl load ~/Library/LaunchAgents/com.agentgate.server.plist
launchctl start com.agentgate.server
tail -f /tmp/agentgate.err.log
```

Replace `YOUR_USER` with your macOS username.

---

## 4) Docker / container

A `Dockerfile` is included in the repository. It builds a Linux binary in a Rust builder image and runs it in a slim Debian runtime.

### Build

```bash
docker build -t agentgate:local .
```

### Run

Create a config on the host first (`./agentgate.toml`). Then run:

```bash
docker run --rm -p 8787:8787 \
  -v $(pwd)/agentgate.toml:/app/agentgate.toml:ro \
  -e RUST_LOG=info \
  agentgate:local \
  /app/agentgate serve --config /app/agentgate.toml --host 0.0.0.0 --port 8787
```

Windows PowerShell:

```powershell
docker run --rm -p 8787:8787 `
  -v ${PWD}\agentgate.toml:/app/agentgate.toml:ro `
  -e RUST_LOG=info `
  agentgate:local `
  /app/agentgate serve --config /app/agentgate.toml --host 0.0.0.0 --port 8787
```

### Docker notes

- The container only contains AgentGate. CLI providers (like `opencode`) must also be installed inside the image/container if you plan to use them.
- Mount any provider credentials/configs required by those CLIs.
- Do not expose port publicly without `server.api_keys`.
- For HTTP-only providers, container deployment is simpler than CLI-provider deployment.

---

## 5) VPS deploy checklist

Use this checklist for a Linux VPS:

- [ ] **Binary installed**: `/usr/local/bin/agentgate` (or known path).
- [ ] **Config placed**: `/etc/agentgate/agentgate.toml`, owned by service user.
- [ ] **API keys set**: `server.api_keys` has strong random keys.
- [ ] **Bind address safe**: `127.0.0.1` + reverse proxy, or `0.0.0.0` only with keys + firewall.
- [ ] **Firewall configured**: allow only required ports (SSH, HTTPS/reverse proxy; avoid exposing raw 8787 publicly).
- [ ] **systemd hardened**: `KillMode=control-group`, `TasksMax=1024`, `MemoryMax=8G`, `Restart=on-failure`.
- [ ] **Provider CLIs installed**: required CLIs on `PATH` for the service user.
- [ ] **Provider secrets set**: env vars/systemd `Environment=`/drop-in files; no secrets in git.
- [ ] **Health check**: `curl http://127.0.0.1:8787/health` returns OK.
- [ ] **Model check**: `curl -H "Authorization: Bearer <key>" http://127.0.0.1:8787/v1/models`.
- [ ] **Logs monitored**: `journalctl -u agentgate -f`.
- [ ] **Update plan**: keep old binary backup, roll out new binary, restart, smoke test.

---

## Verified provider mappings note

Some HTTP providers need exact endpoint/field mappings. Use only mappings marked as verified in docs/config.

- NVIDIA NIM: see [`docs/PROVIDER_NVIDIA_NIM.md`](./docs/PROVIDER_NVIDIA_NIM.md). The example config includes a verified NVIDIA mapping block (disabled by default; set `NVIDIA_API_KEY` and `enabled = true`).
- Before enabling a new HTTP provider mapping, confirm request/response compatibility (especially `tool_choice`, `response_format`, streaming, and model IDs).
- If a provider returns 4xx schema errors, disable unsupported forwarded fields and check the provider-specific doc.

---

## Auth & API key safety

- **Never bind `0.0.0.0` without `server.api_keys`.** A public/unprotected gateway can expose your upstream providers and spend quota.
- Use long random keys; rotate them when shared.
- Prefer reverse proxy TLS (HTTPS) when exposed outside localhost.
- Keep provider credentials in environment variables, not in committed config.
- Restrict firewall access to trusted IPs when possible.

Example safe local config:

```toml
[server]
host = "127.0.0.1"
port = 8787
api_keys = ["local-dev-key"]
```

Example network config:

```toml
[server]
host = "0.0.0.0"
port = 8787
api_keys = ["long-random-key-1", "long-random-key-2"]
```

---

## Troubleshooting

| Symptom | Likely cause | What to check / fix | Docs |
|---|---|---|---|
| `too_many_tools` / tool count rejected | Model/provider has tool-count limits, or free-model tool filtering/caps apply | Reduce tools in request; use a model/provider with higher tool support; check bridge/tool handling settings | [`docs/tool-handling.md`](./docs/tool-handling.md), [`docs/spec_free_model_combo.md`](./docs/spec_free_model_combo.md) |
| `quota_exceeded`, `429`, `provider.free-limit` | Upstream quota/rate limit exhausted | Wait/reset window, switch model, enable combo/failover, or use provider with available quota | [`docs/spec_free_model_combo.md`](./docs/spec_free_model_combo.md), [`docs/TRANSPORT_HARDENING.md`](./docs/TRANSPORT_HARDENING.md) |
| `Broken pipe (os error 32)` | Client disconnected mid-stream, or upstream/stdin/stdout pipe closed | Check client timeout/disconnect behavior; retry request; inspect AgentGate logs for upstream exit; ensure CLI provider is healthy | [`docs/TRANSPORT_HARDENING.md`](./docs/TRANSPORT_HARDENING.md) |
| `ECONNREFUSED` / connection refused | Upstream not running/listening, wrong host/port, or local CLI/bridge not ready | Verify provider endpoint/CLI, check firewall/port, confirm service is running (`systemctl status`, logs), retry after startup | [`docs/TRANSPORT_HARDENING.md`](./docs/TRANSPORT_HARDENING.md), [`docs/LINUX_VPS_DIAGNOSIS.md`](./docs/LINUX_VPS_DIAGNOSIS.md) |
| `401 Unauthorized` | Missing/wrong `Authorization: Bearer ...` key | Match client key to `server.api_keys`; rotate/update config and restart/reload as needed | [`README.md`](./README.md) |
| `model not found` | Model not configured/discovered | Check `/v1/models`, provider config, provider CLI install, and model cache/ID | [`agentgate.example.toml`](./agentgate.example.toml), [`docs/http_gateway.md`](./docs/http_gateway.md) |
| Windows path on Linux gateway (code appears "cut", agent reads via `sed -n`) | Client sent a Windows absolute path (e.g. `C:\Users\...\file.py`) but gateway runs on Linux; path does not exist in Linux workdir (`/tmp/agentgate/sessions/<id>/work`) | Use a path relative to session workdir, inline file content, or run AgentGate on Windows for Windows-host paths. Gateway may emit a warning log when it detects Windows-style paths on non-Windows hosts. | [`docs/CROSS_PLATFORM_PATHS.md`](./docs/CROSS_PLATFORM_PATHS.md) |

### Useful checks

```bash
# Health
curl http://127.0.0.1:8787/health

# Models (with key)
curl http://127.0.0.1:8787/v1/models -H "Authorization: Bearer <key>"

# Linux service logs
journalctl -u agentgate -n 200 --no-pager
```

---

## Related docs

- [README.md](./README.md)
- [agentgate.example.toml](./agentgate.example.toml)
- [docs/http_gateway.md](./docs/http_gateway.md)
- [docs/TRANSPORT_HARDENING.md](./docs/TRANSPORT_HARDENING.md)
- [docs/spec_free_model_combo.md](./docs/spec_free_model_combo.md)
- [docs/CROSS_PLATFORM_PATHS.md](./docs/CROSS_PLATFORM_PATHS.md)
- [docs/BUILD_AND_RELEASE.md](./docs/BUILD_AND_RELEASE.md)
