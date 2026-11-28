# Cross-platform path limitation (Windows client + Linux gateway)

## Summary

When a Windows client (for example AionUI on Windows) sends **Windows absolute paths** in prompts or tool arguments, but AgentGate runs on a **Linux** gateway/VPS, those paths do not exist on the Linux filesystem.

Example:

- Client sends: `C:\Users\EDN\AppData\Roaming\AionUi\...\mcp_server.py`
- Gateway workdir on Linux: `/tmp/agentgate/sessions/<id>/work/`

The Linux CLI cannot open `C:\...`, so the agent may fall back to reading files in chunks (for example `sed -n '60,260p' ...`, `sed -n '260,395p' ...`). This can look like "code was cut", but the actual problem is a **host path mismatch**, not content truncation.

## Symptoms

- Agent commands fail to read a file at a Windows absolute path.
- Agent repeatedly reads a file by line ranges/chunks.
- Output appears incomplete or "cut" when viewed from the client.
- Logs show tool commands targeting Windows-style paths on a Linux host.

## Guidance

| Situation | Recommended fix |
|---|---|
| File is inside the AgentGate session/workdir | Use a **relative path** inside the session workdir, or copy/upload the file into that workdir. |
| You need to pass file contents | **Inline the content** (or a relevant excerpt) in the prompt/message instead of a host-only absolute path. |
| You must use Windows-local paths/tools | Run AgentGate **locally on Windows** (or on a host that can access those paths). |
| Building automation | Normalize paths before sending requests: convert client host paths to gateway-visible paths or session-relative paths. |

## Detection / warning

AgentGate can emit a **warning log** when it detects Windows-style absolute paths in tool arguments while running on a non-Windows gateway (companion code task). Treat that warning as a signal to fix the path, not as automatic path rewriting.

## What AgentGate does not do

AgentGate does **not** blindly rewrite Windows paths to Linux paths. Automatic rewriting is risky because a Windows-looking string may be source code, data, or an intentional literal value. Any future normalization should be limited, explicit, and based on trusted session/workspace mappings.

## Related

- [INSTALL.md](../INSTALL.md) - troubleshooting table
- [README.md](../README.md) - overview and security notes
- [docs/TRANSPORT_HARDENING.md](./TRANSPORT_HARDENING.md) - transport/runtime error behavior