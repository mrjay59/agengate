# agentgate

OpenAI-compatible gateway yang mengeksekusi **tool milik client di sisi client** (bukan di server B), untuk provider `opencode`, `gemini`, `claude`, dan `ollama`. Menjadi **server MCP** bagi CLI agent agar tool bawaan CLI dimatikan dan tool client dipasang via bridge.

## Status
- Build: `cargo build` hijau
- Tests: `cargo test` hijau (53 lib + integration)
- Spec compliance: `docs/spec_tooling.md` ~45–55%, `docs/spec_reasoning.md` ~70–80% (lihat `docs/audit_spec_tooling.md`, `docs/spec_tooling_progress.md`)
- Gap utama: bridge end-to-end (MCP→client tool_calls→resume) belum terhubung penuh; provider adapter masih generik; `is_bridge_tool` perlu perbaikan prefix

## Struktur
- `src/bridge.rs` — BridgeServer, Run, RunState, PendingCall, ToolSchema
- `src/run.rs` — classify, RunRegistry, RunManager
- `src/service.rs` — ServiceRegistry, chat flow, stream events
- `src/provider/` — CliProvider, bridge adapters (opencode/gemini/claude/ollama), ollama native
- `src/reasoning/` — event, parser, coalescer, format, fallback, redact, limit, pipeline, tool_view
- `src/openai/` — tools, sse_tools
- `src/api.rs`, `src/cli.rs`, `src/doctor.rs`, `src/metrics.rs`

## Menjalankan
```bash
cargo build
cargo test
cargo run -- serve
```

## Dokumen
- `docs/spec_tooling.md` — spec tool bridge
- `docs/spec_reasoning.md` — spec reasoning/thinking streaming
- `docs/audit_spec_tooling.md` — audit terbaru
- `docs/spec_tooling_progress.md` — progress tracker
- `plan.md` — rencana umum

## Catatan
File ini dibuat untuk testing pembacaan project dan penulisan file. Update manual jika perlu.
