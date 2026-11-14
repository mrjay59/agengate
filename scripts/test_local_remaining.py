#!/usr/bin/env python3
import requests, json, time

LOCAL = "http://127.0.0.1:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/9router/free-forever"

TOOLS = [
    ("grep", "grep", "search for pattern in files", "Grep"),
    ("edit", "edit", "edit file content", "Edit"),
    ("execute", "execute", "execute command", "Execute"),
    ("bash", "bash", "bash command", "Bash"),
    ("cat", "cat", "cat file", "Cat"),
    ("patch", "patch", "apply patch to file", "Patch"),
    ("task", "task", "run task", "Task"),
    ("todowrite", "todowrite", "write todo item", "Todowrite"),
    ("todoread", "todoread", "read todo list", "Todoread"),
]

for label, tool, prompt, exp in TOOLS:
    td = {"type": "function", "function": {"name": tool, "description": f"{tool} tool", "parameters": {"type": "object", "properties": {}}}}
    p = {"model": MODEL, "messages": [{"role": "user", "content": prompt}], "tools": [td], "tool_choice": "auto", "stream": False, "temperature": 0.1, "max_tokens": 1000}
    try:
        r = requests.post(LOCAL, headers=AUTH, json=p, timeout=30)
        print(f"{label}: http={r.status_code}", end=" ")
        if r.status_code == 200:
            d = r.json()
            tc = d.get("choices", [{}])[0].get("message", {}).get("tool_calls", [])
            if tc:
                n = tc[0].get("function", {}).get("name")
                status = "PASS" if n == exp else "FAIL"
                print(f"emit={n} exp={exp} {status}")
            else:
                print("NO_TOOL")
        else:
            print(f"error: {r.text[:100]}")
    except Exception as e:
        print(f"EXCEPTION: {e}")
    time.sleep(0.3)