#!/usr/bin/env python3
"""
Verify tool casing fix with all tools available on VPS
Since free models are exhausted, test what we can
"""
import requests, json, time

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}

# Test what works on VPS
models_to_test = ["agg/9router/ag-poli", "agg/9router/free-forever"]

TOOL_SPECS = [
    ("shell/bash",   "shell", "run shell ls", "Shell"),
    ("read/cat/view", "read", "read file /etc/hosts", "Read"),
    ("write/create", "write", "create file test.txt", "Write"),
    ("glob/find",   "glob", "find *.txt files", "Glob"),
    ("grep",        "grep", "search for pattern in files", "Grep"),
    ("edit/patch",  "edit", "edit file content", "Edit"),
    ("list/ls",     "list", "list directory contents", "List"),
    ("execute/run/cmd", "execute", "execute command", "Execute"),
    ("bash",        "bash", "bash command", "Bash"),
    ("cat",         "cat", "cat file", "Cat"),
]

all_results = []

print("=" * 70)
print("VERIFY-TOOL-CASING-FULL — VPS Gateway")
print("Note: Free models have quota limits; testing 9router models")
print("=" * 70)

for model in models_to_test:
    print(f"\n{'='*70}")
    print(f"MODEL: {model}")
    print(f"{'='*70}")
    
    model_results = []
    for label, tool_name, prompt, expected_name in TOOL_SPECS:
        tool_def = {"type": "function", "function": {"name": tool_name, "description": f"{tool_name} tool", "parameters": {"type": "object", "properties": {}}}}
        payload = {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "tools": [tool_def],
            "tool_choice": "auto",
            "stream": False,
            "temperature": 0.1,
            "max_tokens": 1000,
        }
        
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
            }
            
            if r.status_code == 200:
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
            
        except Exception as e:
            result = {
                "label": label,
                "tool_requested": tool_name,
                "expected_name": expected_name,
                "http_status": None,
                "mode_header": None,
                "emitted_name": None,
                "status": f"EXCEPTION: {e}",
            }
        
        model_results.append(result)
        emitted = result["emitted_name"] or "none"
        status = result["status"]
        match_str = "✓" if status == "PASS" else ("✗" if status == "FAIL" else "-")
        print(f"  {label:<20} | Req:{tool_name:<8} | Emit:{emitted:<8} | Exp:{expected_name:<8} | {status} [{match_str}]")
        time.sleep(0.5)
    
    all_results.extend(model_results)
    
    # Model summary
    pass_count = sum(1 for r in model_results if r["status"] == "PASS")
    fail_count = sum(1 for r in model_results if r["status"] == "FAIL")
    error_count = sum(1 for r in model_results if r["status"] not in ("PASS", "FAIL"))
    print(f"\n  Summary: PASS={pass_count}, FAIL={fail_count}, ERROR={error_count}")

# Final summary
print("\n" + "=" * 70)
print("FINAL SUMMARY MATRIX")
print("=" * 70)
print(f"{'Tool':<20} | {'Requested':<10} | {'Emitted':<10} | {'Expected':<10} | {'Status'}")
print("-" * 70)

# Group by tool
by_tool = {}
for r in all_results:
    key = r["label"]
    if key not in by_tool:
        by_tool[key] = []
    by_tool[key].append(r)

for tool_label in ["shell/bash", "read/cat/view", "write/create", "glob/find", "grep", "edit/patch", "list/ls", "execute/run/cmd", "bash", "cat"]:
    entries = by_tool.get(tool_label, [])
    for entry in entries:
        emitted = entry["emitted_name"] or "none"
        status = entry["status"]
        match_str = "✓" if status == "PASS" else ("✗" if status == "FAIL" else "-")
        print(f"{entry['label']:<20} | {entry['tool_requested']:<10} | {emitted:<10} | {entry['expected_name']:<10} | {status} [{match_str}]")

import datetime
filename = f"verify_tool_casing_final_{datetime.datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
with open(filename, "w") as f:
    json.dump({"results": all_results}, f, indent=2)
print(f"\nSaved to {filename}")