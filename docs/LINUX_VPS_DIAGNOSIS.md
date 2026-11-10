# Linux VPS diagnosis — "prompting gives no response / no reasoning"

Status: **evidence collected against the live host** (2026-11-10, agentgate v0.1.0 release binary).
Owner: Lead. Related tasks: `LINUX-VPS (Testing)`, `LINUX-VPS (BD)`, `LINUX-VPS (BD2)`.
> The VPS runs the v0.1.0 artifact, i.e. commit `775bdc0`. It does **not** contain `affe25a`
> (cancelled-request kill/cleanup) or later commits. Findings below are tagged where that matters.

---

## 1. Summary

AgentGate on the VPS is **running and functional**. Two independent classes of problems produced the
"no response / no reasoning" symptom:

| # | Root cause | Severity | Owner |
|---|---|---|---|
| C1 | The opencode account's **free quota is exhausted** on the VPS; the upstream error is produced but never reaches the client before the client gives up | Critical (user-visible) | env + BD |
| C2 | AgentGate **does not fail fast**: a provider that errors/hangs results in an SSE stream of `: keep-alive` comments with **no `error` chunk and no `data: [DONE]`** until `request_timeout` (600 s) / bridge turn timeout (~120 s) | Critical | BD |
| C3 | The VPS runs the **legacy `opencode-ai` 1.19.5** CLI, not `@opencode/cli` 2.x; after an upstream stream error it does not exit, so the bridge waits for its own timeout instead of surfacing the error in ~1 s (Windows 2.2.6 surfaces it immediately) | High | BD2 / env |
| C4 | The release Linux binary is built on ubuntu-24.04 and requires **GLIBC_2.39**; the VPS has **glibc 2.35**. Every invocation prints `weak version 'GLIBC_2.39' not found` | High (portability) | BD2 |
| C5 | Dead providers/models are advertised: when a provider scan fails, `/v1/models` falls back to the **static `models` list** from the config, and `agentgate models` (CLI) always prints that static list. Missing ollama models additionally make `ollama run` **auto-pull** multi-GB blobs while the request hangs | High | BD / BD2 |
| C6 | `check_available()` disagrees with actual execution: `agentgate doctor` prints `opencode MISSING` while the same process successfully runs `opencode models` (89 models) and serves opencode requests via the `resolve_binary` nvm fallback (`src/provider.rs:635`) | Medium | BD |
| C7 | Client cancel leaks the child CLI: after the HTTP client disconnects, `ollama run` keeps running (observed 41 s, CPU active). Fixed on `main` by `affe25a`, **not present in the v0.1.0 artifact** | Medium (fixed on main) | — |

Reasoning formatting itself is **not** broken (see §4).

---

## 2. Environment

| Item | Value |
|---|---|
| Host | `10.11.1.21` → `server-ai`, Ubuntu 22.04.5 LTS, kernel 5.15.0-194, x86_64 |
| glibc | `ldd (Ubuntu GLIBC 2.35-0ubuntu3.15) 2.35` |
| Binary | `/usr/local/bin/agentgate` (15,948,808 B), `agentgate 0.1.0` |
| Service | `agentgate.service`, `ExecStart=/usr/local/bin/agentgate serve --host 0.0.0.0 --port 8780`, `Restart=always`, `User=root` |
| Listening | `LISTEN 0 128 0.0.0.0:8780 users:(("agentgate",pid=3606118,fd=19))` |
| Config | `/root/.config/agentgate.toml` (`[server] port = 8787`, overridden by `--port 8780`) |
| Providers on PATH | `ollama` yes; `opencode` **not on PATH** (only via nvm fallback); `gemini` absent; `claude` absent |
| Ollama | 0.32.0 — `nemotron-3-nano:4b`, `qwen2.5-coder:7b`, `llama3:latest`, plus `*-cloud` (account-bound) |
| opencode | `/root/.nvm/versions/node/v20.20.2/bin/opencode` → `../lib/node_modules/opencode-ai/bin/opencode.exe`, **version 1.19.5** |

---

## 3. Evidence

### 3.1 Binary needs a newer glibc than the host

```
$ objdump -T /usr/local/bin/agentgate | grep -o 'GLIBC_[0-9.]*' | sort -u -V | tail -3
GLIBC_2.34
GLIBC_2.39

$ agentgate --help
agentgate: /lib/x86_64-linux-gnu/libc.so.6: weak version `GLIBC_2.39' not found (required by agentgate)
Usage: agentgate [OPTIONS] <COMMAND>
```

The symbol version is *weak*, so the process still starts — but this is fragile and will hard-fail on
older targets (Debian 12 ships 2.36, Ubuntu 22.04 2.35).

### 3.2 The server itself is healthy

```
GET /health                 -> 200
GET /v1/models (no key)     -> 401
GET /v1/models (default_key) -> 98 models
```

### 3.3 The user's failing requests were `opencode/beam-free`

`journalctl -u agentgate` (all `stream=Some(false)`, i.e. non-streaming):

```
11:28:52 Request received: model=agg/opencode/beam-free, stream=Some(false), messages=2
11:28:52 chat_only_run: using native CLI provider provider_id="opencode"
11:28:52 call_provider_native_with: calling send_chat_completion model="opencode/beam-free"
```

The upstream error is logged by opencode itself (`/root/.local/share/opencode/log/opencode.log`):

```
level=ERROR message="stream error" providerID=opencode modelID=beam-free
  error.error="AI_APICallError: Free usage limit reached. Free models work in OpenCode
  (https://opencode.ai); subscribe to OpenCode Go to keep going: https://opencode.ai/go"
level=ERROR message="stream error" providerID=opencode modelID=gpt-5.4-nano
  error.error="AI_APICallError: Insufficient account funds"
```

So the model *was* called, and the provider answered with a quota error. The client never saw it:
after ~2 minutes the gateway only logged

```
11:27:03 ERROR agentgate::service: provider stream error: provider_error: bridge turn timed out
```

preceded by repeated

```
WARN agentgate.bridge.opencode.stdin: No stdin available to write to
WARN agentgate::provider::bridge::opencode: OpenCode process did not exit after stdout EOF; killing it
```

### 3.4 Stream never terminates when the provider cannot answer

Request `agg/gemini/gemini-2.5-flash` (binary absent), `stream:true`, 30 s cap:

```
curl_exit=28 (timeout)   bytes=270   DONE_count=0   "error" present: no
data: {"choices":[{"delta":{"role":"assistant"},"finish_reason":null,"index":0}] ...}
: ping
: keep-alive
: keep-alive
```

Same shape for `ollama/llama3` while `llama3` was not installed: `ollama run llama3` started pulling
**4.7 GB**; the stream emitted only `: ping` / `: keep-alive` for 40 s, never `[DONE]`, never an error.

### 3.5 Positive control — the same binary streams correctly for a reachable model

```
POST /v1/chat/completions {model:"agg/ollama/nemotron-3-nano:4b", stream:true}
data: {... "delta":{"reasoning_content":"User asks: \"Say hello in one short sentence.\" ..."}}
data: {... "delta":{"reasoning_content":"No policy issues.\n"}}
data: {... "delta":{"content":"Hello!\n"}}
data: {"choices":[{"delta":{},"finish_reason":"stop"}]}
data: [DONE]
```

Non-stream returns `message.content`, `message.reasoning_content`, `finish_reason":"stop"`.
**Reasoning delivery is correct**; `usage.completion_tokens` is `0` (minor defect).

### 3.6 Model catalog is inconsistent between CLI and server

```
$ agentgate models            # CLI
total_models=10              # config lists only: llama3, deepseek-r1, phi3, gemini*, opencode*

$ curl /v1/models             # server
total 98                     # real `ollama list` (7) + real `opencode models` (89) + fallbacks
```

`agentgate models` registers only `build_cli_provider` (`src/cli.rs:42`), which for stdio-shim
providers is the bridge adapter that echoes `models` from the config, so it never runs `models_cmd`.
When a scan *fails* the server also falls back to that static list (`src/service.rs:289-296`), which is
how `gemini/*` and `opencode/beam-free` kept being advertised while unusable.

`agentgate doctor` is inconsistent in the opposite direction: it prints `opencode MISSING` (it uses
`which` only) while the running server serves opencode through `resolve_binary`'s nvm fallback.

### 3.7 Verification matrix (v0.1.0 artifact, zero opencode quota used)

| Model | Mode | HTTP | Secs | reasoning_content | content | finish | `[DONE]` | completion_tokens |
|---|---|---|---|---|---|---|---|---|
| `agg/ollama/nemotron-3-nano:4b` | stream | 200 | 16 | 3 chunks | 1 chunk | stop | yes | — |
| `agg/ollama/llama3:latest` | stream | 200 | 9 | none (model has no thinking) | 1 chunk | stop | yes | — |
| `agg/ollama/nemotron-3-nano:4b` | non-stream | 200 | 16 | 175 chars | `Hello!` | stop | — | **0** |
| `agg/ollama/qwen2.5-coder:7b` | non-stream | 200 | 30 | none | `Hello!` | stop | — | **0** |
| `agg/ollama/llama3:latest` | non-stream | 200 | 18 | none | `Hello!` | stop | — | **0** |
| `agg/gemini/gemini-2.5-flash` (binary absent) | stream | 200 | 0 | — | — | error | yes | — |
| `opencode/beam-free` | non-stream | 200 | **~120** | — | — | error | — | — |

Two important nuances:

- A provider that is **not registered at all** (binary absent → `check_available` false) fails fast
  (0 s) with an `error` chunk and `[DONE]`. That path is fine.
- A provider that **is registered but fails at runtime** (opencode quota error, or a missing ollama
  model that triggers a pull) is the path that hangs. This is the user-visible bug.

### 3.8 Cancel does not kill the child CLI

With the v0.1.0 artifact, a client disconnect during streaming leaves the provider process alive:

```
$ timeout 3 curl ... model=agg/ollama/qwen2.5-coder:7b
$ ps -eo pid,ppid,etime,cmd | grep 'ollama run'
3625694 3606118 00:41 Sl  ollama run qwen2.5-coder:7b
  cwd -> /tmp/agentgate/sessions/f78f5989-.../work
```

`affe25a` on `main` wires `notifications/cancelled` to `cancel_session` and fixes this; the released
artifact predates it. Re-verify after the next artifact is built.

---

## 4. What is NOT the problem

- Reasoning format/`reasoning_content` emission: works (see §3.5).
- Ollama CLI execution: works (`scan_models: completed provider=ollama models_found=7 elapsed_ms=174`).
- Server port / firewall / systemd: `0.0.0.0:8780` listening, `/health` 200.
- Config path: correctly loaded from `/root/.config/agentgate.toml`.

---

## 5. Immediate mitigations (no code change)

1. **Do not use `opencode/*` free models on this account** — the free quota is exhausted
   (`Free usage limit reached`). Either subscribe / connect a provider, or use another model.
2. Use ollama for a known-good path:
   `agg/ollama/nemotron-3-nano:4b`, `agg/ollama/qwen2.5-coder:7b`, `agg/ollama/llama3`.
   (`llama3:latest` is now installed on the VPS — it was pulled during this diagnosis.)
3. Put opencode on the service PATH (systemd `Environment=PATH=...`), and upgrade to the CLI AgentGate
   targets (`@opencode/cli` 2.x) instead of the legacy `opencode-ai` 1.19.5.
4. Only list models that exist. Missing ollama models cause a multi-GB auto-pull while the client hangs.

---

## 6. Code fixes required

| ID | File | Change |
|---|---|---|
| C2-a | `src/service.rs:960` (`stream_run_events`) | bound the startup/first-token wait; on timeout send `RunEvent::Error` (the task holds `tx.clone()`, so a hung provider keeps the channel open forever) |
| C2-b | `src/api.rs:347-392` | hard deadline based on the last meaningful provider event; then error chunk + finish chunk + `data: [DONE]`; never an infinite `: keep-alive` loop |
| C2-c | `src/service.rs` provider resolution | if `check_available()` is false, fail with `provider_unavailable` before spawning |
| C2-d | `src/cli.rs` (`test`) | default deadline, currently unbounded |
| C5-a | `src/service.rs:289` | do not fall back to the static config list when the scan failed for a *binary-absent* provider; mark provider unavailable instead |
| C5-b | `src/cli.rs:42` | `agentgate models` must perform the same scan as the server |
| C6 | `src/provider.rs` (`check_available`) and `src/doctor.rs` | use the same `resolve_binary` path as execution so `doctor`/availability agree with reality (currently `opencode MISSING` yet usable) |
| C7 | `src/bridge.rs` (`cancel_session`) | already fixed by `affe25a`; must be included in the next artifact and re-verified on Linux |
| C4 | `.github/workflows/build.yml` | build Linux on ubuntu-22.04 (or musl) so max symbol ≤ GLIBC_2.34 |
| C3 | docs + `INSTALL.md` | state the supported opencode CLI version and how to put it on PATH |

Acceptance for C2: a request that reaches a **registered** provider which then fails must return an
`error` chunk and terminate with `[DONE]` in **≤ 15 s**. Verify with `opencode/beam-free` (quota error)
or a CLI that emits an error event and then stays alive. The binary-absent path already passes (0 s).

---

## 7. Reproduction commands

```bash
# stack
systemctl status agentgate --no-pager
ss -tlnp | grep 8780
objdump -T /usr/local/bin/agentgate | grep -o 'GLIBC_[0-9.]*' | sort -u -V | tail -3

# catalog
agentgate models
curl -s -H 'Authorization: Bearer default_key' http://127.0.0.1:8780/v1/models

# positive control (ollama)
curl -sN -X POST http://127.0.0.1:8780/v1/chat/completions \
  -H 'Content-Type: application/json' -H 'Authorization: Bearer default_key' \
  -d '{"model":"agg/ollama/nemotron-3-nano:4b","messages":[{"role":"user","content":"Say hello"}],"stream":true}'

# failure mode: provider binary absent (must be a fast error, currently 0 byte -> keep-alive only)
curl -sN -m 30 -X POST http://127.0.0.1:8780/v1/chat/completions \
  -H 'Content-Type: application/json' -H 'Authorization: Bearer default_key' \
  -d '{"model":"agg/gemini/gemini-2.5-flash","messages":[{"role":"user","content":"hi"}],"stream":true}'
```

---

## 8. Implementation status (fixes applied on `main`)

| Fix | File | Behaviour |
|---|---|---|
| First-token deadline | `src/api.rs`, `src/service.rs`, `src/config.rs` | SSE stream emits `"code": "first_token_timeout"` + `data: [DONE]` when no provider event arrives within `[defaults] first_token_timeout_secs` (default 120 s). |
| Mid-stream idle deadline | `src/api.rs` | After the first event, `AGENTGATE_STREAM_IDLE_TIMEOUT_SECS` (default 120 s) of silence emits `"code": "provider_timeout"` + `[DONE]`. |
| Terminal provider error | `src/api.rs` | A `RunEvent::Error` now ends the stream immediately instead of waiting for a CLI that never exits (the opencode quota case). |
| Provider availability gate | `src/api.rs`, `src/service.rs`, `src/error.rs` | Unavailable provider → HTTP `503` `{"code":"provider_unavailable"}` before any stream is opened. Ollama-over-HTTP is exempt. |
| Catalog truthfulness | `src/service.rs` | `/v1/models` no longer advertises configured models for an unavailable provider (static fallback kept only when the provider is reachable). |
| CLI/server parity | `src/service.rs` (`register_configured`), `src/main.rs`, `src/cli.rs` | `models`, `test`, `doctor` and `providers` register providers exactly like `serve`, so the CLI no longer prints 10 static models while the server reports 98, and `opencode` is not reported `MISSING`. |
| Non-stream deadline | `src/service.rs` | `send_chat_completion` bounded by `AGENTGATE_PROVIDER_TIMEOUT_SECS` (default 120 s) → `502 provider_error: produced no result within Ns (timed out)`. |
| `test` deadline | `src/main.rs` | `agentgate test` bounded by `AGENTGATE_TEST_TIMEOUT_SECS` (default 180 s). |
| Portable Linux artifact | `.github/workflows/build.yml` | Linux jobs run on `ubuntu-22.04` → max symbol GLIBC_2.35 (was 2.39). |
| Regression tests | `tests/stream_timeout_test.rs` | Silent provider → `first_token_timeout` + `[DONE]`; provider that errors then stays alive → error chunk + `[DONE]` in < 10 s. |

### Local verification (debug build, Linux-equivalent code paths)

```
# unavailable provider (binary not resolvable)
GET  /v1/models                 -> {"object":"list","data":[]}
POST /v1/chat/completions       -> HTTP 503 in 0.019 s
                                   {"error":{"code":"provider_unavailable", ...}}

# provider that errors and does not exit cleanly
POST /v1/chat/completions       -> HTTP 200 in 2.27 s
                                   data: {..., "finish_reason":"error",
                                          "error":{"code":"provider_error", ...}}
                                   data: [DONE]

# healthy ollama model (regression check)
POST agg/ollama/nemotron-3-nano:4b stream=true -> 59 s,
   role=1 reasoning_chunks=1 content_chunks=1 stop=1 done=1
```

`cargo test --lib` → 55 passed; `cargo test --tests` → all suites pass (2 new tests).

### Remaining before this is closed on the VPS

1. Ship a new artifact built on `ubuntu-22.04` and replace `/usr/local/bin/agentgate` (v0.1.0 predates every fix above, including the cancel-kill fix `affe25a`).
2. Re-run the failure-mode matrix on the VPS: unavailable provider must answer `503` in ≤ 1 s; a registered provider that errors must terminate the stream in ≤ 15 s.
3. Environment: put `opencode` on the service `PATH` (systemd `Environment=PATH=...`) and use `@opencode/cli` 2.x instead of the legacy `opencode-ai` 1.19.5, which does not exit after an upstream error.
4. The VPS opencode account free quota is exhausted (`Free usage limit reached`); no gateway change can fix that — use ollama models or another provider until the quota resets/subscription is added.
