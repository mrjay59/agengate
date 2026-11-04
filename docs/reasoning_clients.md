# Reasoning Client / 9router Compatibility (RS7)

No client faithfully renders reasoning until the **whole chain** is verified:

```
agentgate ──► 9router (optional) ──► AionUi / UI / SDK
```

9router may translate formats, compress output, or buffer SSE. A format that
works directly against `agentgate` can still fail once an intermediary is in
front of it. `agentgate doctor --probe-reasoning` exists to measure exactly
that.

## 1. Compatibility matrix

The machine-readable source is [`compat/clients.json`](../compat/clients.json),
embedded into the binary at build time.

Print it with:

```bash
agentgate doctor --matrix
```

Current summary (see the JSON for risks and verification steps):

| Client | Recommended | Fallback | Status |
|---|---|---|---|
| `aionui` | `reasoning_content` | `think_tags` | unverified |
| `deepseek-style-ui` | `reasoning_content` | `both` | known |
| `openrouter-style-ui` | `reasoning` | `both` | known |
| `programmatic-sdk` | `none` | `reasoning_content` | known |

| Intermediary | Preserves reasoning | Buffers SSE | Status |
|---|---|---|---|
| `9router` | unknown | unknown | unverified |

Entries marked `unverified` mean: **do not assume**. Run the probe through the
real chain and record the result in the JSON `decision_log`.

## 2. Synthetic probe

`--probe-reasoning` starts a small HTTP server that streams deterministic SSE
without calling any model:

```bash
agentgate doctor --probe-reasoning
# listens on http://127.0.0.1:8799
```

Options:

| Flag | Default | Meaning |
|---|---|---|
| `--host` | `127.0.0.1` | Bind address |
| `--port` | `8799` | Bind port |
| `--seconds` | `120` | Stop automatically after N seconds |
| `--follow` | `false` | Run until Ctrl+C (ignores `--seconds`) |

Endpoints:

| Path | Purpose |
|---|---|
| `/` | HTML index with links to every probe |
| `/matrix` | Embedded `compat/clients.json` |
| `/health` | `{"status":"ok"}` |
| `/probe/reasoning/:format` | Synthetic stream for one format |
| `/probe/reasoning/think_tags` | Opens/closes `<think>` exactly once |
| `/probe/reasoning/none` | No reasoning fields at all |

Each stream emits:

1. `t=0.0s` — assistant role chunk.
2. `t=0.0s` — reasoning delta #1.
3. `t=0.4s` — reasoning delta #2.
4. `t=0.8s` — text: `probe answer: streaming OK`.
5. `finish_reason: "stop"` and `[DONE]`.

If both reasoning deltas are visible **before** the answer, the chain is
streaming. If they appear together with the answer or not at all, the chain is
buffering, filtering, or rewriting.

## 3. Verification procedure

For each format that looks plausible for your client:

1. Test direct:
   ```bash
   curl -N http://127.0.0.1:8799/probe/reasoning/reasoning_content
   ```
2. Test through 9router with **output compression OFF**.
3. Repeat with **output compression ON**.
4. Test through the actual client (AionUi, SDK, …).
5. Record which format still arrives incrementally in `compat/clients.json`
   (`status`, `verified_with`, `decision_log`).

A quick sanity check that two deltas are separated on the wire:

```bash
curl -N http://127.0.0.1:8799/probe/reasoning/reasoning_content \
  | while IFS= read -r line; do printf '%s %s\n' "$(date +%s.%N)" "$line"; done
```

## 4. Choosing a format

Use the first entry that passes the whole chain:

1. `reasoning_content` — best default.
2. `both` — if the client or intermediary is unknown.
3. `think_tags` — only if the client parses inline tags and is not a
   programmatic consumer.
4. `none` — programmatic clients and bandwidth-sensitive links.

Set it globally in `agentgate.toml`, or override per request:

```bash
curl -N http://127.0.0.1:8787/v1/chat/completions \
  -H 'X-Agentgate-Reasoning: both' \
  -H 'Content-Type: application/json' \
  -d '{"model":"opencode/gpt-5","stream":true,"messages":[{"role":"user","content":"hi"}]}'
```

The response headers echo the effective format/kind/granularity, which makes
misconfiguration visible even when a UI hides the reasoning pane.

## 5. Troubleshooting

| Symptom | Likely cause | Check |
|---|---|---|
| Nothing until the answer | CLI buffered, or intermediary buffered | Direct probe vs. through 9router |
| Reasoning appears with the answer | Producer collects all deltas first | `doctor --probe-reasoning`, then CLI flags |
| `reasoning_content` empty | Client reads `reasoning` only | Try `both` |
| `<think>` tags visible as text | Client has no tag parser | Use `reasoning_content`/`reasoning` |
| Truncated reasoning | `max_chars_per_turn` hit | Raise limit or disable in config |
| Secrets visible | Redaction pattern missing | Add a `redact_patterns` entry |

## 6. Automated checks

```bash
cargo test --test doctor_probe_test
cargo test --test reasoning_hardening
```

The first validates the probe endpoints and embedded matrix; the second
covers limits, redaction, CRLF handling, ordering and parallel streams.
