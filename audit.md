# Audit Mendalam — AgentGate Bridge (Tooling & Reasoning)

**Project:** `C:\Users\EDN\Documents\AION\project\agentgate`
**Flow:** `AionUi (A) → 9router → AgentGate (B, Windows/Linux) → opencode (prompting, builtin disable, MCP bridge)`
**Spec:** `docs/spec_tooling.md` v0.2 (R1–R12, §0–§15), `docs/spec_reasoning.md` RS0–RS5 (§0–§16)
**Tanggal audit:** 2026-11-09 (refresh setelah perbaikan)
**Verifikasi:** `cargo test --lib` 55/55 lulus (unit). Integration bridge **belum end-to-end**.

---

## TRIAGE ADDENDUM (2026-11-10) — P0/P1-AUDIT COMPLETE

**Commits since original audit:**
- `83d8f5f` seq1: SSE §7.4 exactness (2-chunk), cancelled kill+cleanup, MCP hold keepalive
- `7e38aff` + `a0053b3` seq2: hold↔Run live mapping, truncate(1)→batch/queue, tool_call_id generator, NameMap/alias, ID/TTL
- `16ad1f5` seq3: guard fail-closed+prefix (agentgate_*/mcp__agentgate__), .cmd/PATHEXT Windows, transport single source, `--tools ""`
- `ae0aabb` PR-S5: replay session rotation (cli_gen++), cleanup hardening, orphan PID sweep
- `5e35647` CI: build-essential on Linux jobs

**Previously "STILL BROKEN" — NOW FIXED:**
| Item | Was | Now | Commit |
|---|---|---|---|
| SSE tool_calls 2-chunk | single-chunk | 2-chunk + 512B continuation + finish_reason=tool_calls | 83d8f5f |
| notifications/cancelled kill | no-op 202 | BRIDGE.cancel_session → kill child + cleanup workdir + stop run | 83d8f5f |
| MCP hold keepalive | not used | 250ms ticker loop in SSE hold path | 83d8f5f |
| cli.rs raw CliProviderImpl | bypassed bridge | uses build_cli_provider() | (prior) |
| RequestBatcher dead code | truncate(1) active | batch/queue remaining for serial mode | 7e38aff |
| tool_call_id from CLI | parser raw | ToolCallIdGenerator::call_id() in bridge | 7e38aff |
| Guard prefix recognition | weak | agentgate_*/mcp__agentgate__ in guard_patterns | 16ad1f5 |
| .cmd/PATHEXT Windows | not handled | resolve_binary_path() in claude/gemini | 16ad1f5 |
| Transport single source | hardcoded http | {transport}/{transport_url} from agentgate.toml | 16ad1f5 |
| Builtin disable | missing | claude --tools "" + guard_patterns | 16ad1f5 |
| NameMap/limit | partial | full alias→tool_name reverse mapping | 7e38aff |

**Remaining (infrastructure/operational):**
- CI runner: build-essential + x86_64-linux-gnu-gcc (workflow updated 5e35647, re-run needed)
- Target-env E2E (Linux + 9router/AionUI/VSC Copilot) — infra pending
- CI artifacts/checksums — pending CI completion
- Opencode free usage limit — quota reset needed for live eval

**Local test status:** `cargo check` OK, `cargo test --lib` 55/55, `cargo test --tests` all pass (exit 0). WSL Linux build verified green.

## 0. Dukungan Linux vs Windows

| Aspek | Linux | Windows |
|---|---|---|
| `temp_dir()` fallback (claude/gemini) | OK /tmp/agentgate valid | BAD jadi C:\tmp\agentgate di drive berjalan (P1-3) |
| `.cmd` handling (opencode.cmd/claude.cmd) | OK tidak perlu | BAD perlu cmd /C + PATHEXT (P1-3) |
| `Read` folder -> os error 5 | OK harness izinkan listing dir | BAD Access denied (harness hanya file individual) (P1-2) |
| Process tree kill | OK kill -9 | WARN taskkill /T /F perlu impl |
| Path separator / env | OK native | WARN perlu normalize \ vs / |
| MCP stdio shim | OK pipe unix | WARN named pipe Windows |
| opencode/claude/gemini CLI | OK native | WARN .cmd wrapper |

**Kesimpulan Linux:** AgentGate lebih stabil di Linux karena fallback /tmp valid, tidak ada .cmd, dan Read folder tidak error. Namun P0 (hold<->Run disconnected, SSE single-chunk, cancel/keepalive) tetap blocker di semua platform. P1-2 (guard lemah, disable builtin) juga berlaku di Linux — hanya gejala os error 5 yang tidak muncul; CLI masih bisa panggil builtin read folder (berhasil di Linux, tetap melanggar isolasi S1/S3).

**Rekomendasi:** Targetkan Linux sebagai platform utama untuk BR0-BR7 (uji R1 lebih mudah), lalu hardening Windows (BR7) dengan .cmd + temp_dir() + process tree kill.

---

## 1. Ringkasan Eksekutif (Refresh)

| Spec | Estimasi kesesuaian | Status |
|---|---|---|
| docs/spec_tooling.md | 70-75% (naik dari 65-70%) | Fondasi + beberapa P1/P2 sudah fixed; P0 SSE/cancel/keepalive + P2 batcher/id-gen masih broken |
| docs/spec_reasoning.md | ~75% | Tidak berubah; L1 streaming, coalescer, format, parser, ollama native OK; mapping effort + compat/*.json belum |

**Kesimpulan:** Masih belum sesuai untuk production. Perbaikan signifikan (55 test, fallback temp_dir, resolve API, max_result_bytes enforce, Claude strict-mcp-config, Gemini settings file, os error 5 -> JSON) tapi blocker P0 (SSE 2-chunk, cancelled kill, keepalive) dan P2 (batcher, id generator, NameMap) masih ada.

---

## 2. Status Perbaikan (Verified)

### FIXED (terbukti di HEAD)

| Item | Bukti |
|---|---|
| Fallback /tmp -> temp_dir() semua provider | provider.rs:255,388,1488,1690, bridge.rs:774, claude.rs:159, gemini.rs:159, opencode.rs:167, bridge.rs:72 |
| max_result_bytes + max_prompt_chars enforced | bridge.rs:571,623, provider.rs:1171 |
| Claude --mcp-config <file> --strict-mcp-config | claude.rs:187-188 |
| Gemini settings file per-run | gemini.rs:188 |
| Resume -> resolve_tool_result_by_id + continuation + replay rotate | service.rs:659-769 |
| POST /_bridge/resolve/:call_id + auth + tool_already_resolved | api.rs:34,866 |
| os error 5 -> JSON error jelas | api.rs:828-843 |
| Disable builtin opencode aktif | build_opencode_config di request path |
| cargo test --lib | 55/55 lulus (sebelumnya 31) |
| git status | bersih |

### PARTIAL

| Item | Status | Catatan |
|---|---|---|
| Hold<->Run loop | PARTIAL | API resolve ada, tapi associate_tool_call_ids tak pernah dipanggil di live path -> lookup miss -> jatuh ke replay. Hanya opencode via continuation; claude/gemini replay |
| Guard is_bridge_tool / preflight_isolation | PARTIAL | 503 wired, tapi prefix agentgate_* tak dikenali & preflight lemah |
| .cmd handling Windows | PARTIAL | claude/gemini spawn langsung tanpa resolve .cmd |
| Transport stdio_shim vs http | PARTIAL | default http vs agentgate.toml stdio_shim |
| Gemini injeksi | PARTIAL | settings file ok, tapi tanpa excludeTools |
| Claude injeksi | PARTIAL | --strict-mcp-config ok, tapi tanpa --tools "" |
| Resume/TTL | PARTIAL | ada file/code, tapi belum konsisten di jalur utama |
| NameMap/limit | PARTIAL | ada, tapi belum di jalur utama |
| reasoning_effort / compat | PARTIAL | ada file/code, tapi belum di jalur utama |

### STILL BROKEN

| Item | Lokasi | Gejala |
|---|---|---|
| SSE tool_calls single-chunk (bukan 2-chunk sec 7.4) | api.rs:552-574 | kirim id+name+arguments sekaligus + finish. Spec wajib chunk1 {id,name,arguments:""} + chunk2 {id,name:null,arguments:"<json>"} + continuation 512B + finish_reason=tool_calls |
| notifications/cancelled tetap 202 diam | api.rs:766 | tidak batalkan pending/kill child -> zombie + cwd bocor |
| Keepalive tidak dipakai | api.rs:700-709 | endpoint JSON, bukan SSE; keepalive_interval tak dipakai |
| cli.rs masih CliProviderImpl mentah | cli.rs:18,46,73 | bypass bridge -> spawn tanpa MCP, tanpa cwd kosong, tanpa disable builtin |
| RequestBatcher dead code | bridge.rs:14,727 | truncate(1) aktif -> buang tool call paralel |
| tool_call_id dari CLI, bukan generator | parser.rs:384, bridge.rs:368 | lookup miss -> replay (kecuali opencode continuation) |

---

## 3. Gap yang Masih Harus Diperbaiki (Prioritas)

### P0 (kritis, blocker R1)
1. SSE 2-chunk api.rs:552-574 - emit chunk1 {id,name,arguments:""} + chunk2 {id,name:null,arguments} + continuation 512B + finish.
2. cancelled kill api.rs:766 - BRIDGE.cancel_pending(id) + RunService.cancel + Child::kill() + return 200 isError:true.
3. keepalive SSE api.rs:700-709 - select! 15s -> : keepalive\n\n.
4. cli.rs builder cli.rs:18,46,73 - ganti CliProviderImpl::new() -> build_cli_provider().

### P1 (isolasi & Windows)
5. Guard prefix agentgate_* + mcp__agentgate__ dikenali + preflight fail-closed.
6. .cmd handling Windows di claude/gemini spawn.
7. Transport ikut agentgate.toml (bukan hardcode default http).
8. Gemini excludeTools + Claude --tools "".

### P2 (lifecycle)
9. Aktifkan RequestBatcher 150ms (ganti truncate(1)).
10. tool_call_id dari ToolCallIdGenerator::call_id() di semua jalur.
11. NameMap dedupe _2/_3 + enforce limit di render.
12. reasoning_effort mapping + compat/*.json per-CLI.

---

## 4. Roadmap (Refresh)

- Tahap 1 P0: SSE 2-chunk, cancelled kill, keepalive, cli.rs builder. (est. 1 sprint)
- Tahap 2 P1: guard, .cmd, transport, Gemini/Claude injeksi. (est. 1 sprint)
- Tahap 3 P2: batcher aktif, id generator, NameMap, reasoning_effort/compat. (est. 1 sprint)
- Verifikasi: cargo test --lib (55), bridge_mcp_hold_test, spec_scenarios_test, fail_closed_test, E2E R1 di Linux.

---

## 5. Checklist Regression (Refresh)

### E2E wajib lolos
- [ ] SSE tool_calls parse 9router (2-chunk + continuation)
- [ ] tools/call hold >45s + keepalive tidak putus
- [ ] notifications/cancelled matikan pending + cleanup temp/agentgate/sessions/{id}/
- [ ] Batch 2 tool call paralel keduanya ter-resolve, bukan truncate
- [ ] Resume parked tanpa spawn baru (log "parked resumed")
- [ ] Windows: tidak Access denied listing cwd kosong
- [ ] guard.enforce fail-closed: disable tools -> CLI error 503
- [ ] Linux: fallback /tmp valid, tidak ada .cmd issue

### Unit / Integration
- [x] cargo test --lib (55 lulus)
- [ ] cargo test --test bridge_mcp_hold_test -- --nocapture
- [ ] cargo test --test spec_scenarios_test -- --nocapture
- [ ] cargo test --test fail_closed_test -- --nocapture
- [ ] Baru: sse_tool_calls_two_chunk_test.rs, cancel_abort_test.rs, claude_strict_mcp_test.rs

### Auth / Network
- [x] /_bridge/mcp/:session tanpa Bearer -> 401
- [x] /v1/chat/completions n>1 -> 400, tools>128 -> 400
- [ ] Win .cmd spawn sukses, fallback tidak Access denied

---

## 6. Lampiran: File & Line yang Diacu

| Area | Path | Baris relevan |
|---|---|---|
| Run classify | src/run.rs | 108,122,148,190,269,329,386,433 |
| Provider | src/provider.rs | 250,255,317,342,375,388,422,445,682,1103,1171,1309,1360,1382,1410,1429,1488,1690 |
| Service | src/service.rs | 26,52,149,202,245,299,496,499,628-665,659-769,677,703,712,736,748,774,782,828 |
| API | src/api.rs | 33,34,103,165,180,218,243,309,331,349,386,391,464,484,550,552-574,644,678,695,700-709,709,737,766,828-843,866 |
| Bridge MCP | src/bridge.rs | 14,72,175,185,197,368,411,442,513,554,571,623,676,704,727,774 |
| Opencode | src/provider/bridge/opencode.rs | 64,116,166,167,380,475-513,490 |
| Claude | src/provider/bridge/claude.rs | 36,156,159,179,187-188,365,370 |
| Gemini | src/provider/bridge/gemini.rs | 156,159,179,188,370 |
| Ollama | src/provider/ollama_native.rs | - |
| OpenAI tools | src/openai/tools.rs | 183,262,294,392,425-430,478,510,535 |
| Config | src/config.rs | 13,47,92-95,173,292,409 |
| Error | src/error.rs | 98-115 |
| Reasoning | src/reasoning/parser.rs | 31,94,180,236,384 |
| Permission | src/permission/mod.rs | - |
| Tests | tests/bridge_mcp_hold_test.rs, tests/spec_scenarios_test.rs, tests/fail_closed_test.rs | - |
| Docs | docs/spec_tooling.md, docs/spec_reasoning.md | - |
| Config | agentgate.toml, Cargo.toml, compat/clients.json | - |

---

## 7. Penutup

Perbaikan signifikan sudah dilakukan (55 test, fallback temp_dir, resolve API, max_result_bytes enforce, Claude strict-mcp-config, Gemini settings file, os error 5 -> JSON). Namun blocker P0 (SSE 2-chunk, cancelled kill, keepalive) dan P2 (batcher, id generator, NameMap) masih harus ditutup sebelum go-live.

**Kunci production:** tutup loop hold<->Run (P0-1) + SSE 2-chunk (P0-2) + cancelled kill (P0-3) + keepalive (P0-4) + cli.rs builder (P1-1). Tanpa keduanya, setiap perbaikan lain hanya mitigasi prompt yang rapuh.

**Platform:** Linux direkomendasikan untuk BR0-BR7 (uji R1 lebih mudah, /tmp valid, tidak ada .cmd). Windows memerlukan hardening tambahan (.cmd + temp_dir() + process tree kill) sebelum go-live.
