#!/usr/bin/env python3
"""
REPRO-BEAM TARGETED: Confirm bridge mode (tools) triggers free-limit
"""
import json, time, requests
from datetime import datetime

URL = "http://127.0.0.1:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/opencode/beam-free"

# Minimal tool stubs to trigger bridge mode (like AionUI ~91 tools)
TOOLS = [
    {"type": "function", "function": {"name": "read", "description": "Read file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "write", "description": "Write file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "glob", "description": "Glob files", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "grep", "description": "Grep search", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "shell", "description": "Shell command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "execute", "description": "Execute command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "list", "description": "List directory", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "patch", "description": "Patch file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "patch": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "edit", "description": "Edit file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "old_str": {"type": "string"}, "new_str": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "create", "description": "Create file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}}},
]

def call(model, messages, tools, stream, label):
    payload = {
        "model": model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "stream": stream,
        "temperature": 0.1,
        "max_tokens": 500,
    }
    start = time.time()
    r = requests.post(URL, headers=AUTH, json=payload, timeout=30, stream=stream)
    elapsed = time.time() - start
    
    print(f"\n--- {label} ---")
    print(f"HTTP: {r.status_code} | Time: {elapsed:.3f}s")
    print(f"Content-Type: {r.headers.get('content-type')}")
    print(f"x-agentgate-mode: {r.headers.get('x-agentgate-mode')}")
    
    if stream:
        chunks = []
        for line in r.iter_lines():
            if not line:
                continue
            s = line.decode('utf-8')
            chunks.append(s)
            if s == "data: [DONE]":
                break
        print(f"Chunks: {len(chunks)}")
        for c in chunks[:5]:
            print(f"  {c[:200]}")
        return {"http": r.status_code, "chunks": chunks, "mode": r.headers.get('x-agentgate-mode')}
    else:
        print(f"Body: {r.text[:500]}")
        return {"http": r.status_code, "body": r.text, "mode": r.headers.get('x-agentgate-mode')}

print("=" * 60)
print("REPRO-BEAM TARGETED: beam-free with tools (bridge mode)")
print("=" * 60)

msg = [{"role": "user", "content": "List files in current directory"}]

# Non-stream with tools
r1 = call(MODEL, msg, TOOLS, False, "NON-STREAM with tools")

# Stream with tools  
r2 = call(MODEL, msg, TOOLS, True, "STREAM with tools")

print("\n" + "=" * 60)
print("RESULT: Both should show bridge mode + free-limit error")
print("=" * 60)