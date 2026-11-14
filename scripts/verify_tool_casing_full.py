#!/usr/bin/env python3
"""
VERIFY-TOOL-CASING-FULL: Comprehensive tool name casing test on VPS gateway
Tests all opencode built-in tools for title-case fallback
"""
import requests, json

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/opencode/deepseek-v4.1-flash-free"

# Tool definitions with title-case expected names
TOOL_SPECS = [
    ("shell/bash",   {"type": "function", "function": {"name": "shell", "description": "Run shell command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}, "required": ["command"]}}, "run shell ls", "Shell"),
    ("read/cat/view", {"type": "function", "function": {"name": "read", "description": "Read file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}, "required": ["path"]}}, "read file /etc/hosts", "Read"),
    ("write/create", {"type": "function", "function": {"name": "write", "description": "Write file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}, "required": ["path", "content"]}}, "create file test.txt", "Write"),
    ("glob/find",   {"type": "function", "function": {"name": "glob", "description": "Glob files", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}}, "required": ["pattern"]}}, "find *.txt files", "Glob"),
    ("grep",        {"type": "function", "function": {"name": "grep", "description": "Grep search", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}}}, "required": ["pattern"]}}, "search for pattern in files", "Grep"),
    ("edit/patch",  {"type": "function", "function": {"name": "edit", "description": "Edit file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "old_str": {"type": "string"}, "new_str": {"type": "string"}}}, "required": ["path", "old_str", "new_str"]}}, "edit file content", "Edit"),
    ("list/ls",     {"type": "function", "function": {"name": "list", "description": "List directory", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}, "required": ["path"]}}, "list directory contents", "List"),
    ("execute/run/cmd", {"type": "function", "function": {"name": "execute", "description": "Execute command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}, "required": ["command"]}}, "execute command", "Execute"),
    ("bash",        {"type": "function", "function": {"name": "bash", "description": "Bash command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}, "required": ["command"]}}, "bash command", "Bash"),
    ("cat",         {"type": "function", "function": {"name": "cat", "description": "Cat file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}, "required": ["path"]}}, "cat file", "Cat"),
]

results = []

print("=" * 70)
print("VERIFY-TOOL-CASING-FULL — VPS Gateway")
print(f"Model: {MODEL}")
print("=" * 70)

for label, tool_def, prompt, expected_name in TOOL_SPECS:
    tool_name = tool_def["function"]["name"]
    payload = {
        "model": MODEL,
        "messages": [{"role": "user", "content": prompt}],
        "tools": [tool_def],
        "tool_choice": "auto",
        "stream": False,
        "temperature": 0.1,
        "max_tokens": 1000,
    }
    
    print(f"\n--- {label} (request: {tool_name}) ---")
    
    try:
        r = requests.post(VPS, headers=AUTH, json=payload, timeout=60)
        result = {
            "label": label,
            "tool_requested": tool_name,
            "expected_name": expected_name,
            "http_status": r.status_code,
            "mode_header": r.headers.get("x-agentgate-mode"),
            "emitted_name": None,
            "status": "ERROR",
            "raw_body": r.text[:300],
        }
        
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
                        if name:
                            result["emitted_name"] = name
                            if name == expected_name:
                                result["status"] = "PASS"
                            else:
                                result["status"] = "FAIL"
                else:
                    result["status"] = "NO_TOOL"
        except json.JSONDecodeError:
            pass
        
        print(f"HTTP: {result['http_status']} | Mode: {result['mode_header']}")
        print(f"Emitted: {result['emitted_name']} | Expected: {expected_name} | {result['status']}")
        
    except Exception as e:
        result = {
            "label": label,
            "tool_requested": tool_name,
            "expected_name": expected_name,
            "http_status": None,
            "mode_header": None,
            "emitted_name": None,
            "status": f"EXCEPTION: {e}",
            "raw_body": "",
        }
        print(f"EXCEPTION: {e}")
    
    results.append(result)

# Summary table
print("\n" + "=" * 70)
print("SUMMARY MATRIX")
print("=" * 70)
print(f"{'Tool':<20} | {'Requested':<10} | {'Emitted':<10} | {'Expected':<10} | {'Status'}")
print("-" * 70)
pass_count = 0
fail_count = 0
for r in results:
    emitted = r["emitted_name"] or "none"
    expected = r["expected_name"]
    status = r["status"]
    match_str = "✓" if status == "PASS" else ("✗" if status == "FAIL" else "-")
    print(f"{r['label']:<20} | {r['tool_requested']:<10} | {emitted:<10} | {expected:<10} | {status}")
    if status == "PASS":
        pass_count += 1
    elif status == "FAIL":
        fail_count += 1

print("-" * 70)
print(f"PASS: {pass_count} | FAIL: {fail_count} | TOTAL: {len(results)}")

# Save results
import datetime
filename = f"verify_tool_casing_full_{datetime.datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
with open(filename, "w") as f:
    json.dump(results, f, indent=2)
print(f"\nSaved to {filename}")
