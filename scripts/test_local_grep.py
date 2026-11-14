#!/usr/bin/env python3
import requests, json, time

LOCAL = "http://127.0.0.1:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/9router/free-forever"

td = {"type": "function", "function": {"name": "grep", "description": "grep tool", "parameters": {"type": "object", "properties": {}}}}
p = {"model": MODEL, "messages": [{"role": "user", "content": "search for pattern in files"}], "tools": [td], "tool_choice": "auto", "stream": False, "temperature": 0.1, "max_tokens": 1000}
try:
    r = requests.post(LOCAL, headers=AUTH, json=p, timeout=60)
    print(f"grep: http={r.status_code}", end=" ")
    if r.status_code == 200:
        d = r.json()
        tc = d.get("choices", [{}])[0].get("message", {}).get("tool_calls", [])
        if tc:
            n = tc[0].get("function", {}).get("name")
            print(f"emit={n} exp=Grep {'PASS' if n == 'Grep' else 'FAIL'}")
        else:
            print("NO_TOOL")
    else:
        print(f"error: {r.text[:100]}")
except Exception as e:
    print(f"EXCEPTION: {e}")