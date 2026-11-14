# Laporan Pengujian Local Gateway AgentGate (127.0.0.1:8780)

**Tanggal:** 2026-11-11
**Tester:** Testing (slot 01a1bfdf-a75d-74e3-9131-a2dc96e41d09)
**Gateway:** http://127.0.0.1:8780 (PID 27168, agentgate.exe)
**Sumber audit:** audit.md (2026-11-09, refresh 2026-11-10)
**Model E2E:** agg/ollama/nemotron-3-nano:4b (lokal, zero quota)

## 1. Build / Test Status
- cargo test --lib: PASS 55/55
- cargo test --tests: PASS (semua binary integration)
- agentgate doctor: opencode/gemini MISSING (kosmetik, .cmd resolve), ollama OK

## 2. Audit.md Item Verification

### P0 (Blocker R1) — 4/4 FIXED
| # | Item | Lokasi | Status |
|---|---|---|---|
| 1 | SSE tool_calls 2-chunk §7.4 | api.rs:644-680 | ✅ FIXED (Chunk1 empty args + Chunk2+ 512B + finish_reason=tool_calls) |
| 2 | notifications/cancelled kill | api.rs:872-877 | ✅ FIXED (cancel_session -> kill CLI + cleanup workdir) |
| 3 | Keepalive saat MCP hold | bridge.rs:526-555 | ✅ FIXED (biased select! 250ms ticker) |
| 4 | cli.rs pakai build_cli_provider | cli.rs:22 | ✅ FIXED |

### P1 (Isolation & Windows) — 5/5 FIXED
| # | Item | Status |
|---|---|---|
| 5 | Guard prefix agentgate_*/mcp__agentgate__ | ✅ FIXED (claude.rs:81) |
| 6 | .cmd handling Windows (resolve_binary_path) | ✅ FIXED |
| 7 | Transport single source ({transport}/{transport_url}) | ✅ FIXED |
| 8 | Gemini excludeTools + Claude --tools "" | ✅ FIXED |
| 9 | Fail-closed guard 503 | ✅ FIXED |

### P2 (Lifecycle) — 4/5 FIXED
| # | Item | Status |
|---|---|---|
| 10 | RequestBatcher batch/queue (ganti truncate(1)) | ✅ FIXED |
| 11 | tool_call_id dari ToolCallIdGenerator | ✅ FIXED |
| 12 | NameMap/limit | ✅ FIXED |
| 13 | Resume/TTL | ✅ FIXED |
| 14 | Replay session rotation (PR-S5) | ✅ FIXED |
| 15 | reasoning_effort mapping + compat/*.json | ⚠️ PARTIAL (defined, belum wired ke service/run/provider) |

### Infra
| Item | Status |
|---|---|
| CI runner build-essential | ✅ CLOSED |
| CI artifacts/checksums | ✅ CLOSED |
| Target-env E2E (Linux+9router/AionUI) | ⏳ PENDING |
| Opencode free usage limit | ⏳ BLOCKED (quota reset) |

## 3. Local Gateway E2E Matrix (port 8780)

### 3.1 Health & Models
| Test | Result | Detail |
|---|---|---|
| GET /health | ✅ PASS | {"status":"ok"} HTTP 200 |
| GET /v1/models (Bearer default_key) | ✅ PASS | 88 model diiklankan |
| GET /_bridge/mcp/:session (tanpa Bearer) | ✅ PASS | HTTP 401 |
| POST /v1/chat/completions n=2 | ✅ PASS | HTTP 400 |

### 3.2 Streaming (agg/ollama/nemotron-3-nano:4b)
| Test | Result | Detail |
|---|---|---|
| Role chunk | ✅ PASS | delta.role="assistant" |
| reasoning_content chunk(s) | ✅ PASS | streamed |
| content chunk(s) | ✅ PASS | "Hello! 👋 ..." |
| finish_reason=stop | ✅ PASS | |
| literal data: [DONE] | ✅ PASS | |
| Keep-alive tidak ganti terminal | ✅ PASS | 4 baris ping/keep-alive mid-stream, [DONE] tetap di akhir |
| Total time | 30.95s | CPU-only |

### 3.3 Non-Stream (agg/ollama/nemotron-3-nano:4b)
| Test | Result | Detail |
|---|---|---|
| content non-empty | ✅ PASS | "Hi!" |
| reasoning_content present | ✅ PASS | present |
| usage.completion_tokens > 0 | ❌ FAIL (DEFECT) | completion_tokens=0, prompt_tokens=0, total_tokens=0 |

### 3.4 Cancel Test
| Test | Result | Detail |
|---|---|---|
| Start stream, kill client 3s, cek orphan | ✅ PASS | Tidak ada orphan "ollama run" dalam 10s; hanya ollama serve yang tersisa |

### 3.5 agentgate doctor
| Provider | Status | Catatan |
|---|---|---|
| opencode | MISSING (kosmetik) | .cmd resolve (opencode.cmd ada di PATH) |
| gemini | MISSING (kosmetik) | .cmd resolve |
| ollama | OK | Available |

### 3.6 Failure Mode A (gemini)
| Test | Result | Detail |
|---|---|---|
| agg/gemini/gemini-2.5-flash stream | ✅ PASS (fast error) | ~8s, finish_reason=error, error chunk dengan pesan trust dir, diakhiri [DONE]. Tidak hang. |

### 3.7 Failure Mode B (model tidak ada)
| Test | Result | Detail |
|---|---|---|
| agg/ollama/definitely-not-a-model:0b | ✅ PASS (fast error) | ~1.26s, pull manifest gagal cepat, finish_reason=error, diakhiri [DONE]. Tidak ada download nyata. |

## 4. Defect yang Ditemukan
1. **usage.completion_tokens == 0 pada non-stream** (DEFECT) — path ollama bridge tidak mengisi usage tokens. Perlu fix di provider/ollama.
2. **reasoning_effort belum wired** (PARTIAL) — types/config ada tapi tidak dikonsumsi. Prioritas rendah.
3. **doctor melaporkan opencode/gemini MISSING** (kosmetik) — flag availability tidak update setelah .cmd resolve. Minor.

## 5. Verdict Keseluruhan
- Core bridge (P0): 4/4 FIXED & verified.
- Isolation (P1): 5/5 FIXED & verified.
- Lifecycle (P2): 5/6 FIXED (reasoning_effort pending).
- Infra: CI GREEN, artifacts verified.
- Local gateway E2E: Streaming PASS, non-stream PASS (kecuali usage tokens defect), cancel PASS, failure modes PASS (fast error).
- Kepatuhan audit.md: ~90% (reasoning_effort + usage tokens + target-env E2E tersisa).

## 6. Rekomendasi
1. Fix usage token accounting di path provider ollama (non-stream).
2. Wire reasoning_effort ke service/run/provider (prioritas rendah).
3. Fix flag availability doctor untuk provider yang resolve via .cmd.
4. Jalankan target-env E2E di Linux + 9router/AionUI saat infra tersedia.
5. Quota reset dibutuhkan untuk live eval opencode (tidak blocking; constraint zero-quota dihormati).

**Catatan:** Team coordination tools (team_*) tidak dapat digunakan karena error 401 "Invalid API key" dari bridge lokal; laporan ini disampaikan langsung sebagai teks pengganti team_send_message.
