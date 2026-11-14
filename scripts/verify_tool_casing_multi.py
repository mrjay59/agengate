#!/usr/bin/env python3
"""
VERIFY-TOOL-CASING-FULL: Test with model that has quota on VPS
"""
import requests, json, time

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODELS_TO_TRY = [
    "agg/opencode/beam-free",
    "agg/opencode/iceman-free",
]

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

def test_model(model):
    all_results = []
    
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
        
        all_results.append(result)
        time.sleep(0.5)  # Rate limit
    
    return all_results

# Try each model
for model in MODELS_TO_TRY:
    print(f"\n{'='*70}")
    print(f"TESTING MODEL: {model}")
    print(f"{'='*70}")
    
    results = test_model(model)
    
    print(f"{'Tool':<20} | {'Req':<10} | {'Emitted':<10} | {'Expected':<10} | {'Status'}")
    print("-" * 70)
    pass_count = 0
    fail_count = 0
    for r in results:
        emitted = r["emitted_name"] or "none"
        status = r["status"]
        print(f"{r['label']:<20} | {r['tool_requested']:<10} | {emitted:<10} | {r['expected_name']:<10} | {status}")
        if status == "PASS":
            pass_count += 1
        elif status == "FAIL":
            fail_count += 1
    
    print(f"\n{model}: PASS={pass_count}, FAIL={fail_count}, TOTAL={len(results)}")
    
    import datetime
    filename = f"verify_tool_casing_{model.replace('/', '_')}_{datetime.datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
    with open(filename, "w") as f:
        json.dump({"model": model, "results": results}, f, indent=2)
    print(f"Saved to {filename}")