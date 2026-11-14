#!/usr/bin/env python3
"""
Investigate shell tool casing specifically - test different casings in request
"""
import requests, json

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/9router/ag-poli"  # has quota

# Test shell with different request casings
test_cases = [
    ("shell (lowercase)", "shell"),
    ("Shell (title case)", "Shell"),
    ("SHELL (uppercase)", "SHELL"),
]

for label, tool_name in test_cases:
    print(f"\n{'='*60}")
    print(f"TEST: {label}")
    print(f"{'='*60}")
    
    tool_def = {"type": "function", "function": {"name": tool_name, "description": "Run shell command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}}
    payload = {
        "model": MODEL,
        "messages": [{"role": "user", "content": "run shell ls"}],
        "tools": [tool_def],
        "tool_choice": "auto",
        "stream": False,
        "temperature": 0.1,
        "max_tokens": 1000,
    }
    
    r = requests.post(VPS, headers=AUTH, json=payload, timeout=60)
    print(f"HTTP: {r.status_code} | Mode: {r.headers.get('x-agentgate-mode')}")
    
    try:
        data = r.json()
        choices = data.get("choices", [])
        if choices:
            msg = choices[0].get("message", {})
            tool_calls = msg.get("tool_calls", [])
            if tool_calls:
                for tc in tool_calls:
                    fn = tc.get("function", {})
                    name = fn.get("name")
                    args = fn.get("arguments")
                    print(f"  tool_calls[].function.name: '{name}'")
                    print(f"  tool_calls[].function.arguments: '{args}'")
            else:
                print("  No tool_calls")
    except:
        pass
    
    print(f"  Raw response: {r.text[:500]}")

# Also test read for comparison
print(f"\n{'='*60}")
print("COMPARISON: read tool (same model)")
print(f"{'='*60}")

tool_def = {"type": "function", "function": {"name": "read", "description": "Read file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}}
payload = {
    "model": MODEL,
    "messages": [{"role": "user", "content": "read file /etc/hosts"}],
    "tools": [tool_def],
    "tool_choice": "auto",
    "stream": False,
    "temperature": 0.1,
    "max_tokens": 1000,
}

r = requests.post(VPS, headers=AUTH, json=payload, timeout=60)
print(f"HTTP: {r.status_code} | Mode: {r.headers.get('x-agentgate-mode')}")

try:
    data = r.json()
    choices = data.get("choices", [])
    if choices:
        msg = choices[0].get("message", {})
        tool_calls = msg.get("tool_calls", [])
        if tool_calls:
            for tc in tool_calls:
                fn = tc.get("function", {})
                name = fn.get("name")
                args = fn.get("arguments")
                print(f"  tool_calls[].function.name: '{name}'")
                print(f"  tool_calls[].function.arguments: '{args}'")
except:
    pass

print(f"  Raw response: {r.text[:500]}")