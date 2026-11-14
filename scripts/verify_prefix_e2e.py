#!/usr/bin/env python3
"""
VERIFY-PREFIX E2E Test
Tests that tool_calls emitted by gateway have NO namespace prefix
(agentgate_, agentgate__, mcp__<server>_, etc.)
Tests: glob, read, shell, execute, create/write, grep
Models: agg/9router/ag-poli, agg/9router/free-forever
Envs: local (127.0.0.1:8780), VPS (10.11.1.21:8780)
"""

import json
import os
import sys
import time
import requests
from typing import Dict, List, Optional, Tuple

LOCAL_URL = "http://127.0.0.1:8780/v1/chat/completions"
VPS_URL = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODELS = ["agg/9router/ag-poli", "agg/9router/free-forever"]

# Tool test scenarios - each designed to trigger a specific tool
TOOL_SCENARIOS = {
    "glob": {
        "messages": [
            {"role": "user", "content": "List all .rs files in the src directory using glob pattern src/**/*.rs"}
        ],
        "expected_tool": "glob"
    },
    "read": {
        "messages": [
            {"role": "user", "content": "Read the file src/main.rs and show its contents"}
        ],
        "expected_tool": "read"
    },
    "shell": {
        "messages": [
            {"role": "user", "content": "Run shell command: echo 'hello from shell'"}
        ],
        "expected_tool": "shell"
    },
    "execute": {
        "messages": [
            {"role": "user", "content": "Execute command: echo 'hello from execute'"}
        ],
        "expected_tool": "execute"
    },
    "create": {
        "messages": [
            {"role": "user", "content": "Create a file /tmp/test_verify_prefix.txt with content 'test'"}
        ],
        "expected_tool": "create"
    },
    "grep": {
        "messages": [
            {"role": "user", "content": "Search for 'fn main' in the src directory using grep"}
        ],
        "expected_tool": "grep"
    },
}

# Prefixes that should NOT appear in emitted tool names
FORBIDDEN_PREFIXES = [
    "agentgate_",
    "agentgate__",
    "mcp__",
    "opencode_",
]

def call_gateway(url: str, model: str, messages: List[Dict], stream: bool = True) -> Tuple[List[Dict], str]:
    """Call gateway and return tool_calls + raw response"""
    payload = {
        "model": model,
        "messages": messages,
        "stream": stream,
        "temperature": 0.1,
        "max_tokens": 2000,
    }
    r = requests.post(url, headers=AUTH, json=payload, timeout=60, stream=stream)
    
    tool_calls = []
    raw = ""
    
    if stream:
        for line in r.iter_lines():
            if not line:
                continue
            line = line.decode('utf-8')
            raw += line + "\n"
            if line.startswith("data: "):
                data = line[6:]
                if data == "[DONE]":
                    break
                try:
                    chunk = json.loads(data)
                    if "choices" in chunk and chunk["choices"]:
                        delta = chunk["choices"][0].get("delta", {})
                        if "tool_calls" in delta:
                            for tc in delta["tool_calls"]:
                                if tc.get("function", {}).get("name"):
                                    tool_calls.append(tc["function"]["name"])
                except json.JSONDecodeError:
                    pass
    else:
        raw = r.text
        try:
            data = r.json()
            if "choices" in data and data["choices"]:
                msg = data["choices"][0].get("message", {})
                if "tool_calls" in msg:
                    for tc in msg["tool_calls"]:
                        if tc.get("function", {}).get("name"):
                            tool_calls.append(tc["function"]["name"])
        except json.JSONDecodeError:
            pass
    
    return tool_calls, raw

def check_prefixes(tool_name: str) -> List[str]:
    """Return list of forbidden prefixes found in tool_name"""
    found = []
    for prefix in FORBIDDEN_PREFIXES:
        if tool_name.startswith(prefix):
            found.append(prefix)
    return found

def run_test(url: str, env_name: str, model: str, tool_name: str, scenario: Dict) -> Dict:
    """Run single test and return result"""
    print(f"\n  [{env_name}] {model} -> {tool_name}...")
    tool_calls, raw = call_gateway(url, model, scenario["messages"])
    
    result = {
        "env": env_name,
        "model": model,
        "tool_requested": tool_name,
        "tool_calls_emitted": tool_calls,
        "prefix_violations": [],
        "status": "UNKNOWN",
        "raw_snippet": raw[:500] if raw else "",
    }
    
    if not tool_calls:
        result["status"] = "NO_TOOL_CALLS"
        print(f"    ⚠ NO tool_calls emitted")
        return result
    
    for tc in tool_calls:
        violations = check_prefixes(tc)
        if violations:
            result["prefix_violations"].extend(violations)
            print(f"    ❌ VIOLATION: '{tc}' has prefixes: {violations}")
        else:
            print(f"    ✅ CLEAN: '{tc}'")
    
    if result["prefix_violations"]:
        result["status"] = "PREFIX_VIOLATION"
    elif tool_calls:
        result["status"] = "PASS"
    else:
        result["status"] = "NO_TOOL_CALLS"
    
    return result

def main():
    print("=" * 70)
    print("VERIFY-PREFIX E2E Test Suite")
    print("=" * 70)
    
    envs = [
        ("local", LOCAL_URL),
        ("vps", VPS_URL),
    ]
    
    all_results = []
    
    for env_name, url in envs:
        print(f"\n{'='*70}")
        print(f"ENVIRONMENT: {env_name.upper()} ({url})")
        print(f"{'='*70}")
        
        for model in MODELS:
            print(f"\n--- Model: {model} ---")
            for tool_name, scenario in TOOL_SCENARIOS.items():
                try:
                    result = run_test(url, env_name, model, tool_name, scenario)
                    all_results.append(result)
                except Exception as e:
                    print(f"    ❌ ERROR: {e}")
                    all_results.append({
                        "env": env_name,
                        "model": model,
                        "tool_requested": tool_name,
                        "tool_calls_emitted": [],
                        "prefix_violations": [f"ERROR: {e}"],
                        "status": "ERROR",
                        "raw_snippet": str(e)[:200],
                    })
                time.sleep(1)  # Rate limit
    
    # Summary
    print("\n" + "=" * 70)
    print("SUMMARY MATRIX")
    print("=" * 70)
    
    # Group by tool
    for tool_name in TOOL_SCENARIOS.keys():
        print(f"\n--- Tool: {tool_name} ---")
        for env_name in ["local", "vps"]:
            for model in MODELS:
                matches = [r for r in all_results if r["env"] == env_name and r["model"] == model and r["tool_requested"] == tool_name]
                if matches:
                    r = matches[0]
                    status = r["status"]
                    if r["prefix_violations"]:
                        status += f" (violations: {r['prefix_violations']})"
                    tc_str = ", ".join(r["tool_calls_emitted"]) if r["tool_calls_emitted"] else "none"
                    print(f"  {env_name:6} | {model:25} | {status:20} | emitted: [{tc_str}]")
    
    # Overall
    pass_count = sum(1 for r in all_results if r["status"] == "PASS")
    fail_count = sum(1 for r in all_results if r["status"] == "PREFIX_VIOLATION")
    error_count = sum(1 for r in all_results if r["status"] in ("ERROR", "NO_TOOL_CALLS"))
    total = len(all_results)
    
    print(f"\n{'='*70}")
    print(f"OVERALL: {pass_count}/{total} PASS, {fail_count} PREFIX_VIOLATION, {error_count} ERROR/NO_TOOL")
    print(f"{'='*70}")
    
    # Save detailed results
    with open("verify_prefix_results.json", "w") as f:
        json.dump(all_results, f, indent=2)
    print("\nDetailed results saved to verify_prefix_results.json")
    
    # Exit code
    sys.exit(1 if fail_count > 0 else 0)

if __name__ == "__main__":
    main()