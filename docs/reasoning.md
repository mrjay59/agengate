# Reasoning / Thinking Streaming

This document describes the reasoning pipeline implemented in
`src/reasoning/` and the `[reasoning]` configuration block. It covers the
RS6–RS8 milestones from `docs/spec_reasoning.md`:

| Milestone | Scope |
|---|---|
| RS6 | Activity fallback, omitted-thinking marker, `id`/`en` labels |
| RS7 | Client / 9router compatibility matrix and `doctor --probe-reasoning` |
| RS8 | Hard limits, redaction, Windows line endings, parallel/soak safety |

## 1. Data flow

```
CLI stdout ──► ReasoningParser ──► ReasoningPipeline ──► Coalescer ──► SseWriter ──► client
                                   │
                                   ├─ fallback activity (RS6)
                                   ├─ omitted marker     (RS6)
                                   ├─ redaction          (RS8)
                                   └─ limits/truncation  (RS8)
```

* `parser.rs` reads NDJSON / `stream-json` lines from a CLI, normalizes CRLF,
  drops malformed JSON and lines larger than 4 MiB.
* `pipeline.rs` is transport agnostic: it enforces ordering, emits fallback
  activity, redacts text and applies per-turn limits.
* `coalesce.rs` batches tiny deltas while guaranteeing that reasoning is
  flushed and closed before the first text or tool call.
* `format.rs` renders the configured wire format. Only `think_tags` needs
  state (exactly one `<think>` / `</think>` pair per block).

## 2. Configuration

```toml
[reasoning]
enabled = true
format = "reasoning_content"      # reasoning_content | reasoning | both | think_tags | none | passthrough
null_content = true               # emit "content": null during reasoning deltas
early_role_chunk = true           # send role chunk before the CLI responds
flush_ms = 40
flush_bytes = 256
max_chars_per_turn = 200000
max_blocks_per_turn = 64
fallback = "activity"             # activity | none | induce
activity_via_reasoning = true
activity_args = false
activity_min_interval_ms = 1500
activity_lang = "id"              # id | en
activity_prefix = "⏳ "
omitted_marker = "activity"       # activity | none
redact_patterns = [
  '(?i)api[_-]?key\s*[:=]\s*\S+',
  'sk-[A-Za-z0-9_-]{16,}',
]
```

Format precedence (highest first):

1. `X-Agentgate-Reasoning` request header.
2. Request body: `reasoning.exclude: true` or `reasoning.format`.
3. `[reasoning].format` in `agentgate.toml`.

Responses always advertise the effective choice:

```
X-Agentgate-Reasoning-Format: reasoning_content
X-Agentgate-Reasoning-Kind: activity
X-Agentgate-Reasoning-Granularity: block   # "final" for non-streaming
```

## 3. Activity fallback (RS6)

The gateway **never fabricates model thoughts**. When a CLI does not expose
thinking, it may instead send honest gateway activity, e.g.:

```
⏳ Menyiapkan agen…
⏳ Meminta tool: write_file
⏳ Menunggu hasil tool dari klien…
```

* `fallback = "none"` disables all activity.
* `fallback = "induce"` is experimental and never the default.
* `activity_via_reasoning = false` keeps activity out of the reasoning channel.
* Labels are deduplicated, rate-limited by `activity_min_interval_ms`, and the
  generic "model is working" label is emitted only after a quiet period and
  only if no real reasoning/text was observed.
* `activity_lang` selects Indonesian (`id`, default) or English (`en`).
* `activity_args` is reserved for including tool arguments; only the tool name
  is shown by default because arguments frequently contain secrets.

## 4. Omitted-thinking marker (RS6)

Some providers expose a thinking block whose contents are hidden
(`redacted_thinking`, empty `thinking` payload). With
`omitted_marker = "activity"` the client receives a single, localized label:

```
⏳ Model berpikir (isi disembunyikan oleh penyedia)
```

No reasoning delta is emitted. Signatures and ciphertext are dropped. Use
`omitted_marker = "none"` to stay silent.

## 5. Wire formats

| `format` | Streaming field | Notes |
|---|---|---|
| `reasoning_content` | `delta.reasoning_content` | Default; most OpenAI-compatible UIs |
| `reasoning` | `delta.reasoning` | OpenRouter-style UIs |
| `both` | both fields | Safest unknown-client fallback, doubles bytes |
| `think_tags` | `delta.content` `<think>…</think>` | For UIs that only parse inline tags |
| `none` | none | Programmatic clients / bandwidth-sensitive |
| `passthrough` | both + think tags | Debugging only |

`think_tags` is stateful: the opening tag is emitted once per block and the
closing tag before the first text/tool call/`[DONE]`. Literal `</think>`
sequences inside reasoning are escaped with a zero-width space so they cannot
close the block early.

## 6. Hard limits (RS8)

* `max_chars_per_turn` — characters accepted per assistant turn. Truncation is
  UTF-8 safe (never splits a scalar value) and emits a single marker
  (`… [reasoning dipotong]` / `… [reasoning truncated]`).
* `max_blocks_per_turn` — later reasoning blocks are dropped.
* The SSE channel between the producer and the HTTP response is bounded
  (`mpsc::channel(128)`) so a slow client creates backpressure instead of
  unbounded memory growth.
* Parser input lines are capped at 4 MiB.

## 7. Redaction (RS8)

`redact_patterns` are compiled once per response. Matching text is replaced
with `[redacted]`. Invalid regexes are ignored so a bad config cannot fail a
request.

> Redaction is **best-effort**, not a security boundary. It reduces accidental
> exposure of API keys, file contents and tool arguments in reasoning. Full
> reasoning is never logged. Do not rely on it to keep secrets out of a
> compromised client.

## 8. Windows / CRLF (RS8)

* Parser lines are trimmed of `\r\n` before JSON parsing.
* All reasoning and text payloads pass through `normalize_line_endings`, so
  `\r\n` and lone `\r` become `\n` before they reach clients.
* ANSI stripping happens on CLI stdout before parsing.
* Binary/non-UTF-8 output is handled lossily by the line reader; malformed JSON
  lines are skipped rather than forwarded as assistant text.

## 9. Ordering guarantees

1. A reasoning block is always started before its deltas.
2. `ReasoningEnd` is emitted before the first text delta.
3. `</think>` is emitted before `tool_calls` and before `[DONE]`.
4. Usage, when present, is emitted before the final finish chunk.

These are covered by `tests/reasoning_hardening.rs`.

## 10. Verifying a client chain (RS7)

See [reasoning_clients.md](reasoning_clients.md) for the client/9router
matrix and the `agentgate doctor --probe-reasoning` workflow.
