#!/usr/bin/env python3
"""
VERIFY-TOOL-CASING-FULL: Streaming mode test (worked before)
"""
import requests, json, time

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/opencode/beam-free"

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

for label, tool_name, prompt, expected_name in TOOL_SPECS:
    tool_def = {"type": "function", "function": {"name": tool_name, "description": f"{tool_name} tool", "parameters": {"type": "object", "properties": {}}}}
    payload = {
        "model": MODEL,
        "messages": [{"role": "user", "content": prompt}],
        "tools": [tool_def],
        "tool_choice": "auto",
        "stream": True,
        "temperature": 0.1,
        "max_tokens": 1000,
    }
    
    print(f"\n--- {label} (request: {tool_name}) ---")
    
    try:
        r = requests.post(VPS, headers=AUTH, json=payload, timeout=60, stream=True)
        
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
                                result["emitted_name"] = name
                                if name == expected_name:
                                    result["status"] = "PASS"
                                else:
                                    result["status"] = "FAIL"
                except:
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
    
    all_results.append(result)
    emitted = result["emitted_name"] or "none"
    status = result["status"]
    print(f"HTTP: {result['http_status']} | Mode: {result['mode_header']} | Emitted: {emitted} | Expected: {expected_name} | {status}")
    time.sleep(1)

# Summary
print("\n" + "=" * 70)
print("STREAMING SUMMARY")
print("=" * 70)
print(f"{'Tool':<20} | {'Req':<10} | {'Emitted':<10} | {'Expected':<10} | {'Status'}")
print("-" * 70)
pass_count = 0
fail_count = 0
for r in all_results:
    emitted = r["emitted_name"] or "none"
    status = r["status"]
    print(f"{r['label']:<20} | {r['tool_requested']:<10} | {emitted:<10} | {r['expected_name']:<10} | {status}")
    if status == "PASS":
        pass_count += 1
    elif status == "FAIL":
        fail_count += 1

print(f"\nPASS: {pass_count} | FAIL: {fail_count} | TOTAL: {len(all_results)}")

import datetime
filename = f"verify_tool_casing_stream_{datetime.datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
with open(filename, "w") as f:
    json.dump({"model": MODEL, "results": all_results}, f, indent=2)
print(f"Saved to {filename}")