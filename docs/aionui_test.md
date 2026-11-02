# Testing AionUI CLI – Prompting

## 1. Start the AgentGate server

```
cargo run --release serve --host 0.0.0.0 --port 8787
```

## 2. Use the AionCLI explorer
AionUI has a built‑in CLI open‑in‑browser tool (look for the *Explorer* panel).
• Navigate to: **http://localhost:8787/v1/health** – you should get `{"status":"ok"}`.
• Click **Try API** and choose **POST /v1/chat/completions**.

## 3. Build a request payload
```json
{
  "model": "opencode/beam-free",
  "messages": [
    {"role": "user", "content": "Hello"}
  ]
}
```

## 4. Submit the request
Copy the JSON into the form, click **Send**. AionCLI will show the full HTTP request and the JSON response.

## 5. Verify latency & response
The response JSON must contain:
- `id`
- `model`
- `choices[].message.content` – e.g. `"Hi. What's the task?"`.

If AionCLI times‑out, increase the *request timeout* in `agentgate.toml` (e.g. `request_timeout_secs = 120`) and restart.

## 6. Automated test (optional)
Create a small Rust script `tests/aionui_test.rs` that uses `reqwest` to POST the payload and assert the response. Run with `cargo test --test aionui_test`.
```
```