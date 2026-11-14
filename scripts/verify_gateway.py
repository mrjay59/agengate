"""End-to-end verification for the agentgate gateway.

Usage:
    python scripts/verify_gateway.py [--host 127.0.0.1] [--port 8787] [--key default_key]

Checks:
  1. GET  /v1/models
  2. POST /v1/chat/completions  opencode/beam-free (chat only)
  3. POST /v1/chat/completions  ollama/nemotron-3-nano:4b (chat only, must have content)
  4. POST /v1/chat/completions  opencode/beam-free with tools (must not hang)
"""

import argparse
import json
import sys
import time

import requests


def post(host, port, key, payload, timeout):
    url = f"http://{host}:{port}/v1/chat/completions"
    started = time.time()
    try:
        resp = requests.post(
            url,
            headers={"Content-Type": "application/json", "Authorization": f"Bearer {key}"},
            json=payload,
            timeout=timeout,
        )
        elapsed = time.time() - started
        try:
            body = resp.json()
        except ValueError:
            body = resp.text
        return resp.status_code, body, elapsed, None
    except Exception as exc:  # noqa: BLE001 - report any transport failure
        return None, None, time.time() - started, f"{type(exc).__name__}: {exc}"


def summarize(body):
    if not isinstance(body, dict):
        return str(body)[:500]
    choice = (body.get("choices") or [{}])[0]
    message = choice.get("message") or {}
    return json.dumps(
        {
            "finish_reason": choice.get("finish_reason"),
            "content": message.get("content"),
            "tool_calls": message.get("tool_calls"),
            "error": body.get("error"),
        },
        ensure_ascii=False,
    )[:800]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument("--key", default="default_key")
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()

    failures = []

    print("== 1. GET /v1/models ==")
    try:
        resp = requests.get(
            f"http://{args.host}:{args.port}/v1/models",
            headers={"Authorization": f"Bearer {args.key}"},
            timeout=30,
        )
        ids = [m.get("id") for m in resp.json().get("data", [])]
        print(f"status={resp.status_code} models={len(ids)}")
        print("\n".join(ids))
        if resp.status_code != 200:
            failures.append("models endpoint did not return 200")
    except Exception as exc:  # noqa: BLE001
        print(f"ERROR {type(exc).__name__}: {exc}")
        failures.append("models endpoint failed")

    cases = [
        (
            "opencode/beam-free chat-only",
            {
                "model": "opencode/beam-free",
                "messages": [{"role": "user", "content": "Halo, siapa kamu? Jawab singkat."}],
                "stream": False,
            },
            "opencode",
        ),
        (
            "ollama chat-only",
            {
                "model": "ollama/nemotron-3-nano:4b",
                "messages": [{"role": "user", "content": "Say hello in one short sentence."}],
                "stream": False,
            },
            "ollama",
        ),
        (
            "opencode/beam-free with tools",
            {
                "model": "opencode/beam-free",
                "messages": [{"role": "user", "content": "What is the weather in Jakarta?"}],
                "stream": False,
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "description": "Get weather",
                            "parameters": {
                                "type": "object",
                                "properties": {"location": {"type": "string"}},
                                "required": ["location"],
                            },
                        },
                    }
                ],
            },
            "opencode",
        ),
    ]

    for index, (name, payload, kind) in enumerate(cases, start=2):
        print(f"\n== {index}. POST {name} ==")
        status, body, elapsed, err = post(
            args.host, args.port, args.key, payload, args.timeout
        )
        if err:
            print(f"ERROR after {elapsed:.1f}s: {err}")
            failures.append(f"{name}: transport error")
            continue
        print(f"status={status} elapsed={elapsed:.1f}s")
        print(summarize(body))
        if elapsed >= args.timeout - 1:
            failures.append(f"{name}: timed out")
        if kind == "ollama":
            content = (
                ((body.get("choices") or [{}])[0].get("message") or {}).get("content")
                if isinstance(body, dict)
                else None
            )
            if not content:
                failures.append(f"{name}: empty content")
        if kind == "opencode":
            # Upstream is rate-limited right now; the gateway must surface an
            # error instead of returning 200 with an empty assistant message.
            content = (
                ((body.get("choices") or [{}])[0].get("message") or {}).get("content")
                if isinstance(body, dict)
                else None
            )
            if status == 200 and not content:
                failures.append(f"{name}: 200 with empty content (error swallowed)")

    print("\n== RESULT ==")
    if failures:
        print("FAIL")
        for item in failures:
            print(f"- {item}")
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
