#!/usr/bin/env python3
"""
Test resume flow with tool_call_id to verify casing + ID handling
"""
import requests, json

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}

TOOLS = [
    {"type": "function", "function": {"name": "shell", "description": "Run shell", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "read", "description": "Read file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
]

def test_full_flow(model, tool_name, prompt):
    print(f"\n=== Full flow test: {tool_name} on {model} ===")
    
    # Step 1: Initial request
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "tools": TOOLS,
        "tool_choice": "auto",
        "stream": True,
        "temperature": 0.1,
        "max_tokens": 2000,
    }
    
    r = requests.post(VPS, headers=AUTH, json=payload, timeout=60, stream=True)
    print(f"Initial HTTP: {r.status_code} | Mode: {r.headers.get('x-agentgate-mode')}")
    
    tool_calls = []
    tool_call_ids = {}
    finish_reason = None
    
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
                    idx = tc.get("index", 0)
                    if idx not in tool_call_ids:
                        tool_call_ids[idx] = {}
                    if "id" in tc:
                        tool_call_ids[idx]["id"] = tc["id"]
                    fn = tc.get("function", {})
                    if "name" in fn:
                        tool_call_ids[idx]["name"] = fn["name"]
            fr = choices[0].get("finish_reason")
            if fr:
                finish_reason = fr
        except:
            pass
    
    # Build tool calls
    for idx in sorted(tool_call_ids.keys()):
        entry = tool_call_ids[idx]
        if "name" in entry and "id" in entry:
            tool_calls.append({"name": entry["name"], "id": entry["id"]})
    
    if not tool_calls:
        print(f"No tool calls emitted. Finish: {finish_reason}")
        return None
    
    tc = tool_calls[0]
    print(f"Emitted tool: {tc['name']} | ID: {tc['id']} | Finish: {finish_reason}")
    
    # Step 2: Resume with role:tool
    resume_payload = {
        "model": model,
        "messages": [
            {"role": "user", "content": prompt},
            {"role": "assistant", "tool_calls": [{"id": tc["id"], "type": "function", "function": {"name": tc["name"], "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": tc["id"], "content": "Command output: success"}
        ],
        "tools": TOOLS,
        "tool_choice": "auto",
        "stream": True,
        "temperature": 0.1,
        "max_tokens": 2000,
    }
    
    r2 = requests.post(VPS, headers=AUTH, json=resume_payload, timeout=60, stream=True)
    print(f"Resume HTTP: {r2.status_code} | Mode: {r2.headers.get('x-agentgate-mode')}")
    
    for line in r2.iter_lines():
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
            fr = choices[0].get("finish_reason")
            if fr:
                print(f"Resume finish_reason: {fr}")
                break
        except:
            pass
    
    return {"tool": tool_name, "emitted": tc["name"], "resume_ok": True}

# Test both models
for model in ["agg/opencode/beam-free", "agg/opencode/iceman-free"]:
    print(f"\n{'='*60}")
    print(f"MODEL: {model}")
    print(f"{'='*60}")
    
    test_full_flow(model, "shell", "List files in current directory")
    test_full_flow(model, "read", "Read the file /etc/hostname")