# Pre-Release Verification Checklist — AgentGate

**Project:** `C:\Users\EDN\Documents\AION\project\agentgate`
**Date:** 2026-11-09
**Scope:** Safe scope — no changes to `src/api.rs`, `src/bridge.rs`, `src/service.rs`, `src/run.rs`, `src/provider.rs`

---

## Prerequisites

- [ ] Windows 10/11 x86_64 (host dev environment)
- [ ] Rust stable toolchain (`rustc`, `cargo`)
- [ ] PowerShell 7+
- [ ] For Linux cross-builds: Linux runner with `gcc` / `musl-gcc` / `aarch64-linux-gnu-gcc`

---

## 1. Code Verification

```powershell
# Clean previous builds to avoid file-lock contention
cargo clean

# Check all targets compile (no runtime execution)
cargo check --all-targets

# Run reasoning unit tests (fast, no linker needed)
cargo test --lib reasoning -- --nocapture

# Run all lib tests
cargo test --lib
```

**Expected:** `cargo check` exits 0; reasoning tests 34/34 pass.

> **Note:** `cargo test --lib` (full) may hit Windows file-lock on `target/debug/agentgate.exe`. If locked, stop any running `agentgate.exe` process and retry.

---

## 2. Release Build

```powershell
cargo build --release
```

**Expected:** `target/release/agentgate.exe` produced.

---

## 3. Smoke Test

```powershell
# Binary starts and shows help
.\target\release\agentgate.exe --help

# Version output
.\target\release\agentgate.exe --version
```

---

## 4. All-Platform Build

```powershell
.\build-all-platforms.ps1 -AllTargets
```

**Expected artifacts:**

| Platform | Artifact |
|---|---|
| Windows x86_64 | `dist/x86_64-pc-windows-msvc/agentgate.exe` |
| Windows aarch64 | `dist/aarch64-pc-windows-msvc/agentgate.exe` |
| Linux x86_64 | `dist/x86_64-unknown-linux-gnu/agentgate` |
| Linux aarch64 | `dist/aarch64-unknown-linux-gnu/agentgate` |

---

## 5. Checksums

```powershell
Get-ChildItem dist/ -Recurse -Filter agentgate* | Get-FileHash -Algorithm SHA256
```

Save to `dist/CHECKSUMS.sha256`.

---

## 6. E2E Blockers (documented — not runnable in dev env)

| Check | Blocker | Required |
|---|---|---|
| 9router/AionUI grouped `thought complete`/`view steps N` | No target env access | 9router + AionUI deploy |
| 9router/AionUI SSE `tool_calls` 2-chunk (§7.4) parsing | No target env access | 9router + AionUI deploy |
| Windows process-tree kill E2E | Windows file-lock contention | Windows CI runner |
| P0-1b ID mapping (mcp_id ↔ tool_call_id) resume E2E | P0-1b Stage 2 in progress | Post P0-1b complete |
| `guard.enforce` fail-closed 503 E2E | P1-2/P1-6 not implemented | Post P1 implementation |

---

## 7. Sign-off

| Item | Status | Sign-off |
|---|---|---|
| `cargo check` pass | ☐ | |
| `cargo test --lib` (core suites) | ☐ | Lead-verified locally: lib 55, bridge_session_registration 8, spec_scenarios 8, sse_tool_calls_contract 6, fail_closed 5, bridge_mcp_hold 1, bridge_resolve_auth 2 (2026-11-09). Full workspace per S5 claim: 137. |
| Release binary smoke test | ☐ | Windows clean rebuild recommended for patched tree |
| All-platform artifacts | ☐ | CI/cross-toolchain (S6) |
| Checksums generated | ☐ | |
| E2E gaps acknowledged | ☐ | |

---

## 7. Stage 2 / S4–S6 E2E (target env required)

- [ ] **S4 live continuation (opencode, capability-gated):** tool_calls park → MCP holds resolved with real results → same CLI session continues (no respawn happy path) → resume HTTP response returns continuation output; on loss/timeout/error → `replay_fallback` reason/header.
- [ ] **Replay fallback:** `parked_session_lost`/`continuation_not_wired`/`continuation_timeout`/`continuation_channel_closed` reasons observable; session rotation (fresh session) on replay.
- [ ] **MCP hold >45s** with keepalive (P0-4) — no disconnect.
- [ ] **Cancel/cleanup:** `notifications/cancelled` + process-tree kill/wait; no leaked runs/continuations/provider sessions/workdirs.
- [ ] **Batch 2 tool calls** parallel resolved (not truncated) — E2E (RequestBatcher not fully wired; limits/dedupe in place).
- [ ] **Linux:** `/tmp`/tempdir, no `.cmd`, BR0–BR7.
- [ ] **9router/AionUI:** SSE 2-chunk `tool_calls`, UX `thought/view steps` grouping, reasoning/tool flow.
- [ ] **VSC Copilot / 9router:** workdir/installer-path + tool flow (original issues).

**Known gaps:** streaming continuation partial; multi-step continuation re-park/state; RequestBatcher not wired to real flow + queue-remaining semantics; reverse alias collision; ID/TTL end-to-end; uncommitted working tree.

---

## 8. Files Referenced

| File | Purpose |
|---|---|
| `build-all-platforms.ps1` | Build script (host-only; needs cross-target support) |
| `agentgate.toml` | Runtime config (server, providers, reasoning) |
| `compat/claude-1.0.0.json` | Claude capability mapping |
| `compat/opencode-1.0.0.json` | OpenCode capability mapping |
| `compat/gemini-1.0.0.json` | Gemini capability mapping |
| `compat/ollama-1.0.0.json` | Ollama native proxy mapping |
| `docs/tooling_conformity_matrix.md` | Full spec compliance matrix |
| `docs/BUILD_AND_RELEASE.md` | Full build/release notes |
