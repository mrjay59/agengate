#!/usr/bin/env python3
import requests, json

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}

# Test tool name casing through gateway VPS
# Model: agg/opencode/beam-free (or iceman-free if quota ready)

TOOLS = [
    {"type": "function", "function": {"name": "shell", "description": "Run shell command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "read", "description": "Read file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "glob", "description": "Glob files", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "grep", "description": "Grep search", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "write", "description": "Write file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "edit", "description": "Edit file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "old_str": {"type": "string"}, "new_str": {"type": "string"}}}}},
]

TOOL_PROMPTS = {
    "shell": "List files in current directory",
    "read": "Read the file /etc/hostname",
    "glob": "List all Python files in the current directory",
    "grep": "Search for 'fn main' in src directory",
    "write": "Create a test file /tmp/test_case.txt with content 'hello'",
    "edit": "Edit /tmp/test_case.txt replacing 'hello' with 'world'",
}

results = []

for tool_name, prompt in TOOL_PROMPTS.items():
    print(f"\n=== Testing {tool_name} ===")
    
    payload = {
        "model": "agg/opencode/beam-free",
        "messages": [{"role": "user", "content": prompt}],
        "tools": TOOLS,
        "tool_choice": "auto",
        "stream": True,
        "temperature": 0.1,
        "max_tokens": 2000,
    }
    
    try:
        r = requests.post(VPS, headers=AUTH, json=payload, timeout=60, stream=True)
        
        result = {
            "tool": tool_name,
            "http_status": r.status_code,
            "mode_header": r.headers.get("x-agentgate-mode"),
            "emitted_tools": [],
            "finish_reason": None,
            "error": None,
        }
        
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
                            result["emitted_tools"].append(name)
                fr = choices[0].get("finish_reason")
                if fr:
                    result["finish_reason"] = fr
                if "error" in chunk.get("choices", [{}])[0]:
                    result["error"] = chunk["choices"][0]["error"]
            except json.JSONDecodeError:
                pass
        
        # Check if casing matches
        result["casing_match"] = result["emitted_tools"] == [tool_name] if result["emitted_tools"] else None
        result["status"] = "PASS" if result["casing_match"] else "FAIL" if result["emitted_tools"] else "NO_TOOL"
        
    except Exception as e:
        result = {
            "tool": tool_name,
            "http_status": None,
            "mode_header": None,
            "emitted_tools": [],
            "finish_reason": None,
            "error": str(e),
            "casing_match": None,
            "status": "ERROR",
        }
    
    results.append(result)
    print(f"HTTP: {result['http_status']} | Mode: {result['mode_header']}")
    print(f"Emitted: {result['emitted_tools']} | Expected: {tool_name} | Match: {result['casing_match']}")
    print(f"Finish: {result['finish_reason']} | Status: {result['status']}")

# Summary table
print("\n" + "=" * 70)
print("VERIFY-TOOLS-CASE — SUMMARY MATRIX")
print("=" * 70)
print(f"{'Tool':<10} | {'Emit Name':<15} | {'Expected':<10} | {'Match':<6} | {'Status'}")
print("-" * 70)
for r in results:
    emit = r["emitted_tools"][0] if r["emitted_tools"] else "none"
    match = "✓" if r["casing_match"] else ("✗" if r["casing_match"] is not None else "-")
    print(f"{r['tool']:<10} | {emit:<15} | {r['tool']:<10} | {match:<6} | {r['status']}")

# Save results
import datetime
filename = f"verify_tools_case_{datetime.datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
with open(filename, "w") as f:
    json.dump(results, f, indent=2)
print(f"\nSaved to {filename}")
