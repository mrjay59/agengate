#!/usr/bin/env python3
"""
Test with multiple tools (like earlier successful test)
"""
import requests, json

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/opencode/beam-free"

# Use multiple tools like earlier successful test
TOOLS = [
    {"type": "function", "function": {"name": "shell", "description": "Run shell", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "read", "description": "Read file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "glob", "description": "Glob files", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "grep", "description": "Grep search", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "write", "description": "Write file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "edit", "description": "Edit file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "old_str": {"type": "string"}, "new_str": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "list", "description": "List directory", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "execute", "description": "Execute command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "bash", "description": "Bash command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "cat", "description": "Cat file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
]

payload = {
    "model": MODEL,
    "messages": [{"role": "user", "content": "List files and read hostname"}],
    "tools": TOOLS,
    "tool_choice": "auto",
    "stream": True,
    "temperature": 0.1,
    "max_tokens": 2000,
}

r = requests.post(VPS, headers=AUTH, json=payload, timeout=60, stream=True)
print(f"HTTP: {r.status_code} | Mode: {r.headers.get('x-agentgate-mode')}")

for line in r.iter_lines():
    if not line:
        continue
    sline = line.decode("utf-8")
    if not sline.startswith("data: "):
        continue
    data = sline[6:]
    if data == "[DONE]":
        break
    print(data[:300])