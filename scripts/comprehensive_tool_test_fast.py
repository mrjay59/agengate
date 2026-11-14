#!/usr/bin/env python3
"""Fast comprehensive AgentGate tooling test - short timeouts, sequential."""
import requests, json, datetime

LOCAL = "http://127.0.0.1:8780/v1/chat/completions"
VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}

MODELS = {
    "LOCAL": ["agg/9router/ag-poli", "agg/9router/free-forever"],
    "VPS": ["agg/9router/ag-poli", "agg/9router/free-forever", "agg/opencode/deepseek-v4.1-flash-free"],
}

TOOL_SPECS = [
    ("shell", "run ls", "Shell"),
    ("read", "read /etc/hosts", "Read"),
    ("write", "create /tmp/t.txt content hi", "Write"),
    ("glob", "find *.txt", "Glob"),
    ("grep", "search 'pattern' /etc", "Grep"),
    ("edit", "edit file content", "Edit"),
    ("list", "list /tmp", "List"),
    ("execute", "run echo test", "Execute"),
    ("bash", "run ls", "Bash"),
    ("cat", "cat /etc/passwd", "Cat"),
    ("patch", "patch file", "Patch"),
    ("task", "run task", "Task"),
    ("todowrite", "write todos", "TodoWrite"),
    ("todoRead", "read todos", "TodoRead"),
]

def make_body(model, prompt, expected, tool_key):
    return {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "tools": [{"type": "function", "function": {
            "name": expected,
            "description": tool_key,
            "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}}
        }}],
    }

def test_one(env, url, model, tool_key, prompt, expected):
    body = make_body(model, prompt, expected, tool_key)
    try:
        r = requests.post(url, headers=AUTH, json=body, timeout=10)
        data = r.json()
        tc = data.get("choices", [{}])[0].get("message", {}).get("tool_calls", [])
        emitted = tc[0]["function"]["name"] if tc else None
        status = "PASS" if emitted == expected else ("NO_TOOL" if emitted is None else "FAIL")
        return {"env": env, "model": model, "tool": tool_key, "expected": expected, "emitted": emitted,
                "http": r.status_code, "status": status}
    except Exception as e:
        return {"env": env, "model": model, "tool": tool_key, "expected": expected, "emitted": None,
                "http": None, "status": f"ERROR: {e}"}

results = []
for env, models in MODELS.items():
    url = LOCAL if env == "LOCAL" else VPS
    print(f"\n=== {env} ===")
    for model in models:
        print(f"\n--- {model} ---")
        for tool_key, prompt, expected in TOOL_SPECS:
            res = test_one(env, url, model, tool_key, prompt, expected)
            results.append(res)
            tag = {"PASS": "OK", "FAIL": "BAD", "NO_TOOL": "NONE"}.get(res["status"], "ERR")
            print(f"  {tool_key:<12} -> {str(res['emitted']):<12} expected {expected:<10} [{tag}]")

total = len(results)
passes = sum(1 for x in results if x["status"] == "PASS")
fails = sum(1 for x in results if x["status"] == "FAIL")
none = sum(1 for x in results if x["status"] == "NO_TOOL")
errs = total - passes - fails - none
print(f"\n{'='*50}")
print(f"OVERALL: {passes}/{total} PASS | {fails} FAIL | {none} NO_TOOL | {errs} ERROR")
for r in results:
    if r["status"] != "PASS":
        print(f"  {r['env']} {r['model']} {r['tool']}: {r['status']} emitted={r['emitted']} expected={r['expected']}")