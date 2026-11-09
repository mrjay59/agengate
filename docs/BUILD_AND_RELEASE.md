# Build & Release Notes — AgentGate

**Project:** `C:\Users\EDN\Document\AION\project\agentgate`
**Date:** 2026-11-09
**Toolchain:** Rust toolchain (cargo/rustc), PowerShell 7+, Windows 10/11 x86_64

---

## 1. Current Build Toolchain

| Component | Version/Requirement | Notes |
|---|---|---|
| `rustc` / `cargo` | Stable (Edition 2021) | `Cargo.toml` edition 2021 |
| Target (host) | `x86_64-pc-windows-msvc` | Current dev environment |
| Linker | Microsoft MSVC linker | Default on Windows |
| PowerShell | 7+ | `build-all-platforms.ps1` |
| Dependencies | 37 crates (`Cargo.toml`) | Includes `tokio`, `axum`, `serde`, `reqwest`, etc. |
| Dev deps | `tokio-test`, `assert_cmd`, `predicates`, `wiremock` | Test-only |

---

## 2. `build-all-platforms.ps1` — Analysis

The existing `build-all-platforms.ps1` was reviewed. Key findings:

**What it does:**
- Builds the AgentGate binary using `cargo build`
- Targets host platform (`x86_64-pc-windows-msvc`)
- Emits binary to `target/debug/agentgate.exe` (debug) or `target/release/agentgate.exe` (release)

**Missing / Not Covered:**
1. **No cross-compilation targets:** Only builds for host `x86_64-pc-windows-msvc`. No `aarch64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.
2. **No rustup target installation:** No `rustup target add` for cross-compile targets.
3. **No cross-linker config:** No `.cargo/config.toml` for Linux cross-linkers (e.g., `gcc-mingw-w64`, `x86_64-linux-gnu-gcc`).
4. **No CI runner matrix:** No GitHub Actions / CI pipeline definition exists.
5. **No artifact release:** No release signing, checksums, or upload step.
6. **Windows-only:** Script references `agentgate.exe`, assumes Windows environment.

**Previous in-session run timed out / no releases because:**
- `cargo check` / `cargo test` on Windows hit file-lock contention (`target/debug/agentgate.exe` locked by running process).
- Full E2E tests require 9router / AionUI target environments not present in dev environment.
- No CI matrix to parallelize platform builds → single-threaded sequential builds time out under Windows file-lock contention.

---

## 3. All-Platform Build Strategy

### 3.1 Target Triples

| Platform | Target Triple | Cross-linker | Runner |
|---|---|---|---|
| Windows x86_64 | `x86_64-pc-windows-msvc` | MSVC (native) | Windows |
| Windows aarch64 | `aarch64-pc-windows-msvc` | MSVC ARM64 | Windows ARM64 |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `gcc` / `musl-gcc` | Linux x86_64 |
| Linux aarch64 | `aarch64-unknown-linux-gnu` | `aarch64-linux-gnu-gcc` | Linux aarch64 |

### 3.2 Required Setup

```powershell
# 1. Install cross-compile targets
rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu

# 2. Install cross-linkers (Linux build from Windows / vice versa)
# Windows: install mingw-w64 for Linux cross-link
# Linux: install musl-cross-make or gcc-mingw-w64 for Windows cross-link

# 3. Configure .cargo/config.toml for linker per target
```

### 3.3 `.cargo/config.toml` Example (Cross-linker)

See `.cargo/config.toml` (created 2026-11-09):

```toml
# Cross-compilation linker configuration

# Linux x86_64 (from Windows host)
[target.x86_64-unknown-linux-gnu]
linker = "x86_64-linux-gnu-gcc"

# Linux aarch64 (from Windows host)
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"

# Windows aarch64 (from Linux host)
[target.aarch64-pc-windows-msvc]
linker = "lld-link.exe"
# ar = "llvm-ar.exe"

# Windows x86_64 (native, no linker config needed on Windows)
# [target.x86_64-pc-windows-msvc]
# linker = "lld-link.exe"
```

### 3.4 Recommended CI Matrix (GitHub Actions)

See `.github/workflows/build.yml` (created 2026-11-09).

```yaml
# .github/workflows/build.yml
strategy:
  matrix:
    target:
      - x86_64-pc-windows-msvc
      - aarch64-pc-windows-msvc
      - x86_64-unknown-linux-gnu
      - aarch64-unknown-linux-gnu
    os:
      - windows-latest
      - ubuntu-latest
```

**Key constraint:** Windows x86_64/aarch64 builds **must run on Windows runners** (MSVC linker). Linux targets **must run on Linux runners** (GNU libc / musl). Cannot cross-compile Windows targets from Linux without Wine + MSVC linker.

---

## 4. Build Script Improvements (Recommended)

Update `build-all-platforms.ps1` to:

1. Accept `-Target` parameter (default host triple)
2. Install target via `rustup target add` if missing
3. Build release profile with `--release`
4. Emit artifacts to `dist/<target>/agentgate(.exe)`
5. Generate SHA256 checksums
6. Fail fast if `cargo check` fails

---

## 5. Compat Template Validation

### 5.1 `compat/*.json` vs `agentgate.toml`

| compat field | agentgate.toml equivalent | Status |
|---|---|---|
| `client` / `binary` | `[[provider]] id` / `bin` | ✅ Matches (opencode/gemini/ollama) |
| `transport` | `[[provider]] transport` | ✅ opencode/gemini use `stdio_shim`, ollama uses `native_http` |
| `temp_dir` | `[defaults] workdir` | ✅ Consistent (`temp_dir()/agentgate/sessions/{id}/`) |
| `mcp_config_method` | Not in agentgate.toml | Documented only (runtime loader missing) |
| `reasoning_effort` mapping | `[reasoning] format = "reasoning_content"` | ✅ Consistent |

### 5.2 compat/*.json vs spec_tooling/reasoning expectations

| Spec Expectation | compat coverage | Gap |
|---|---|---|
| §7.4 SSE `tool_calls` 2-chunk | ✅ Documented in all templates | — |
| §9.6 `notifications/cancelled` abort | ✅ Documented | — |
| §10.4 Resume parked run | ✅ Documented | — |
| §11.3 Empty cwd per session | ✅ `temp_dir` field | — |
| §12.2 OpenCode disable + MCP | ✅ opencode template | — |
| §12.3 Gemini settings file per-run | ✅ gemini template | — |
| §12.4 Claude `--mcp-config --strict-mcp-config` | ✅ claude template | — |
| RS5 `reasoning_effort` mapping | ✅ All templates have mapping | Runtime loader not yet implemented |

**Noted gap:** No runtime loader consumes `compat/*.json` at agentgate startup. Templates are documentation/validation artifacts only. Runtime will hardcode provider behavior until a loader is added (P1/P2 scope).

---

## 6. Pre-Release Verification Checklist

Create `scripts/pre_release_checklist.md`:

### Prerequisites
- [ ] Windows 10/11 x86_64 (host)
- [ ] Rust stable toolchain installed
- [ ] PowerShell 7+
- [ ] (Linux builds) Linux runner with cross-linkers

### Steps

```powershell
# 1. Clean and check
cargo clean
cargo check --all-targets

# 2. Run unit tests
cargo test --lib reasoning -- --nocapture

# 3. Build release for host platform
cargo build --release

# 4. Verify binary starts (smoke test)
.\target\release\agentgate.exe --help

# 5. Build all platforms (via build-all-platforms.ps1)
.\build-all-platforms.ps1 -AllTargets

# 6. Verify artifacts exist
Get-ChildItem dist/ -Recurse

# 7. Generate checksums
Get-FileHash dist/**/agentgate* -Algorithm SHA256
```

### E2E Blockers (documented, not runnable in dev env)

| Check | Blocker |
|---|---|
| 9router/AionUI grouped `thought complete`/`view steps N` | No target env access |
| 9router/AionUI SSE `tool_calls` 2-chunk parsing | No target env access |
| Windows process-tree kill E2E | Windows file-lock contention |
| P0-1b ID mapping resume E2E | P0-1b Stage 2 in progress |

---

## 7. Files Changed

| File | Action | Purpose |
|---|---|---|
| `docs/BUILD_AND_RELEASE.md` | Created | Build/CI notes for all-platform artifacts |
| `docs/tooling_conformity_matrix.md` | Updated | Added P1d build/CI section refs |
| `scripts/pre_release_checklist.md` | Created | Pre-release verification checklist |
