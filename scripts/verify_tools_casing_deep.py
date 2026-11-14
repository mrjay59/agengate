#!/usr/bin/env python3
"""
Deep test: how does gateway handle different casing in request tools?
Test: request with "Shell" vs "shell" → what gets emitted?
"""
import requests, json

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/opencode/beam-free"

def test_with_tool_name(tool_name_in_request):
    """Send request with specific tool name casing, return emitted name"""
    payload = {
        "model": MODEL,
        "messages": [{"role": "user", "content": "Run ls command"}],
        "tools": [{
            "type": "function",
            "function": {
                "name": tool_name_in_request,
                "description": "Run shell command",
                "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}
            }
        }],
        "tool_choice": "auto",
        "stream": True,
        "temperature": 0.1,
        "max_tokens": 1000,
    }
    
    r = requests.post(VPS, headers=AUTH, json=payload, timeout=60, stream=True)
    
    emitted = []
    for line in r.iter_lines():
        if not line:
            continue
        sline = line.decode("utf-8")
        if not sline.startswith("data: "):
            continue
        data = sline[6:]
        if data == "[DONE]":
            break
        try:
            chunk = json.loads(data)
            choices = chunk.get("choices", [])
            if not choices:
                continue
            delta = choices[0].get("delta", {})
            if "tool_calls" in delta:
                for tc in delta["tool_calls"]:
                    fn = tc.get("function", {})
                    name = fn.get("name")
                    if name:
                        emitted.append(name)
        except:
            pass
    
    return r.status_code, r.headers.get("x-agentgate-mode"), emitted

# Test different casings
test_cases = ["shell", "Shell", "SHELL", "read", "Read", "glob", "Glob", "grep", "Grep", "write", "Write"]

print("=" * 70)
print("CASING TEST: Request tool name vs Emitted tool name")
print("=" * 70)

results = []
for tc in test_cases:
    status, mode, emitted = test_with_tool_name(tc)
    emit_str = emitted[0] if emitted else "NONE"
    match = "✓" if emitted and emitted[0].lower() == tc.lower() else "✗" if emitted else "-"
    print(f"Request: {tc:<8} → HTTP {status} | Mode {mode} | Emit: {emit_str:<8} | Match: {match}")
    results.append({
        "request_name": tc,
        "http_status": status,
        "mode": mode,
        "emitted": emit_str,
        "match": match
    })

# Also test: what if multiple tools with mixed casing?
print("\n" + "=" * 70)
print("MULTI-TOOL TEST: Mixed casing in single request")
print("=" * 70)

mixed_tools = [
    {"type": "function", "function": {"name": "Shell", "description": "Run shell", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "Read", "description": "Read file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "Glob", "description": "Glob files", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}}}},
]

payload = {
    "model": MODEL,
    "messages": [{"role": "user", "content": "List files and read hostname"}],
    "tools": mixed_tools,
    "tool_choice": "auto",
    "stream": True,
    "temperature": 0.1,
    "max_tokens": 1000,
}

r = requests.post(VPS, headers=AUTH, json=payload, timeout=60, stream=True)
print(f"HTTP: {r.status_code} | Mode: {r.headers.get('x-agentgate-mode')}")

emitted_multi = []
for line in r.iter_lines():
    if not line:
        continue
    sline = line.decode("utf-8")
    if not sline.startswith("data: "):
        continue
    data = sline[6:]
    if data == "[DONE]":
        break
    try:
        chunk = json.loads(data)
        choices = chunk.get("choices", [])
        if not choices:
            continue
        delta = choices[0].get("delta", {})
        if "tool_calls" in delta:
            for tc in delta["tool_calls"]:
                fn = tc.get("function", {})
                name = fn.get("name")
                if name:
                    emitted_multi.append(name)
    except:
        pass

print(f"Emitted: {emitted_multi}")