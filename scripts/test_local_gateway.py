#!/usr/bin/env python3
import requests, json

url = "http://127.0.0.1:8780/v1/chat/completions"
headers = {"Authorization": "Bearer default_key", "Content-Type": "application/json"}

tools = []
for name in ["shell", "read", "write", "glob", "grep", "list", "patch", "edit", "execute", "create"]:
    tools.append({
        "type": "function",
        "function": {
            "name": name,
            "description": f"Tool {name}",
            "parameters": {"type": "object", "properties": {}}
        }
    })

payload = {
    "model": "agg/opencode/beam-free",
    "messages": [{"role": "user", "content": "hi"}],
    "tools": tools,
    "tool_choice": "auto",
    "stream": False,
    "temperature": 0.1,
    "max_tokens": 500,
}

r = requests.post(url, headers=headers, json=payload, timeout=30)
print("HTTP:", r.status_code)
print("Headers:", dict(r.headers))
print("Body:", r.text[:2000])