#!/usr/bin/env python3
"""
VERIFY-VPS: Reasoning & Tool Emission on VPS Gateway
Tests:
1. reasoning_content / reasoning delta streaming in SSE (bypass 9router)
2. tool_calls emitted with canonical names (no prefix) on VPS
"""

import json
import sys
import time
import requests
from typing import Dict, List, Tuple

VPS_URL = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODELS = ["agg/9router/ag-poli", "agg/9router/free-forever"]

# Test scenarios
REASONING_SCENARIOS = [
    {
        "name": "reasoning_chat",
        "messages": [
            {"role": "user", "content": "Think step by step: what is 2+2? Explain your reasoning."}
        ],
        "expect_reasoning": True,
    },
    {
        "name": "reasoning_complex",
        "messages": [
            {"role": "user", "content": "Reason through this: If all Bloops are Razzies and all Razzies are Lazzies, are all Bloops Lazzies? Show your thinking."}
        ],
        "expect_reasoning": True,
    },
]

TOOL_SCENARIOS = {
    "read": {
        "messages": [{"role": "user", "content": "Read the file /etc/hostname and show its contents"}],
        "expected_tool": "read",
    },
    "glob": {
        "messages": [{"role": "user", "content": "List all .rs files in /root/src directory using glob pattern"}],
        "expected_tool": "glob",
    },
}

FORBIDDEN_PREFIXES = ["agentgate_", "agentgate__", "mcp__", "opencode_"]

def call_gateway(url: str, model: str, messages: List[Dict], stream: bool = True, headers: Dict = None) -> Tuple[Dict, str]:
    payload = {
        "model": model,
        "messages": messages,
        "stream": stream,
        "temperature": 0.1,
        "max_tokens": 3000,
    }
    h = AUTH.copy()
    if headers:
        h.update(headers)
    
    r = requests.post(url, headers=h, json=payload, timeout=90, stream=stream)
    
    result = {
        "reasoning_content_deltas": [],
        "reasoning_deltas": [],
        "tool_calls": [],
        "finish_reason": None,
        "content_deltas": [],
        "raw_lines": [],
    }
    
    if stream:
        for line in r.iter_lines():
            if not line:
                continue
            line = line.decode('utf-8')
            result["raw_lines"].append(line)
            if line.startswith("data: "):
                data = line[6:]
                if data == "[DONE]":
                    break
                try:
                    chunk = json.loads(data)
                    if "choices" in chunk and chunk["choices"]:
                        delta = chunk["choices"][0].get("delta", {})
                        if "reasoning_content" in delta and delta["reasoning_content"]:
                            result["reasoning_content_deltas"].append(delta["reasoning_content"])
                        if "reasoning" in delta and delta["reasoning"]:
                            result["reasoning_deltas"].append(delta["reasoning"])
                        if "content" in delta and delta["content"]:
                            result["content_deltas"].append(delta["content"])
                        if "tool_calls" in delta:
                            for tc in delta["tool_calls"]:
                                fn = tc.get("function", {})
                                name = fn.get("name")
                                if name:
                                    result["tool_calls"].append(name)
                                args = fn.get("arguments")
                                if args:
                                    result["tool_calls"].append(f"{name}({args})")
                        fc = chunk["choices"][0].get("finish_reason")
                        if fc:
                            result["finish_reason"] = fc
                except json.JSONDecodeError:
                    pass
    else:
        result["raw_lines"] = [r.text]
        try:
            data = r.json()
            if "choices" in data and data["choices"]:
                msg = data["choices"][0].get("message", {})
                if "reasoning_content" in msg:
                    result["reasoning_content_deltas"].append(msg["reasoning_content"])
                if "reasoning" in msg:
                    result["reasoning_deltas"].append(msg["reasoning"])
                if "tool_calls" in msg:
                    for tc in msg["tool_calls"]:
                        if tc.get("function", {}).get("name"):
                            result["tool_calls"].append(tc["function"]["name"])
                result["finish_reason"] = data["choices"][0].get("finish_reason")
        except:
            pass
    
    return result, r.text[:1000]

def check_prefixes(tool_name: str) -> List[str]:
    found = []
    for prefix in FORBIDDEN_PREFIXES:
        if tool_name.startswith(prefix):
            found.append(prefix)
    return found

def run_test(url: str, model: str, scenario: Dict, test_type: str) -> Dict:
    print(f"\n  [{model}] {test_type}: {scenario['name']}...")
    result, raw = call_gateway(url, model, scenario["messages"])
    
    violations = []
    for tc in result["tool_calls"]:
        v = check_prefixes(tc)
        if v:
            violations.extend(v)
            print(f"    ❌ VIOLATION: '{tc}' has prefixes: {v}")
        else:
            print(f"    ✅ CLEAN tool: '{tc}'")
    
    has_reasoning_content = len(result["reasoning_content_deltas"]) > 0
    has_reasoning = len(result["reasoning_deltas"]) > 0
    
    if has_reasoning_content:
        print(f"    📝 reasoning_content deltas: {len(result['reasoning_content_deltas'])} (total chars: {sum(len(x) for x in result['reasoning_content_deltas'])})")
        print(f"      First: {result['reasoning_content_deltas'][0][:100]}...")
    if has_reasoning:
        print(f"    📝 reasoning deltas: {len(result['reasoning_deltas'])} (total chars: {sum(len(x) for x in result['reasoning_deltas'])})")
        print(f"      First: {result['reasoning_deltas'][0][:100]}...")
    if not has_reasoning_content and not has_reasoning:
        print(f"    ⚠ No reasoning_content or reasoning deltas observed")
    
    print(f"    finish_reason: {result['finish_reason']}")
    
    return {
        "model": model,
        "test_type": test_type,
        "scenario": scenario["name"],
        "tool_calls": result["tool_calls"],
        "prefix_violations": violations,
        "reasoning_content_count": len(result["reasoning_content_deltas"]),
        "reasoning_content_chars": sum(len(x) for x in result["reasoning_content_deltas"]),
        "reasoning_count": len(result["reasoning_deltas"]),
        "reasoning_chars": sum(len(x) for x in result["reasoning_deltas"]),
        "finish_reason": result["finish_reason"],
        "has_reasoning": has_reasoning_content or has_reasoning,
        "raw_snippet": raw[:500],
    }

def main():
    print("=" * 70)
    print("VERIFY-VPS: Reasoning & Tool Emission (bypass 9router)")
    print(f"Endpoint: {VPS_URL}")
    print("=" * 70)
    
    all_results = []
    
    for model in MODELS:
        print(f"\n{'='*70}")
        print(f"MODEL: {model}")
        print(f"{'='*70}")
        
        # Reasoning tests
        print("\n--- REASONING TESTS ---")
        for scenario in REASONING_SCENARIOS:
            try:
                res = run_test(VPS_URL, model, scenario, "reasoning")
                all_results.append(res)
            except Exception as e:
                print(f"    ❌ ERROR: {e}")
                all_results.append({
                    "model": model, "test_type": "reasoning", "scenario": scenario["name"],
                    "tool_calls": [], "prefix_violations": [f"ERROR: {e}"],
                    "reasoning_content_count": 0, "reasoning_content_chars": 0,
                    "reasoning_count": 0, "reasoning_chars": 0,
                    "finish_reason": None, "has_reasoning": False,
                    "raw_snippet": str(e)[:200],
                })
            time.sleep(2)
        
        # Tool tests
        print("\n--- TOOL EMISSION TESTS ---")
        for tool_name, scenario in TOOL_SCENARIOS.items():
            try:
                res = run_test(VPS_URL, model, scenario, "tool")
                all_results.append(res)
            except Exception as e:
                print(f"    ❌ ERROR: {e}")
                all_results.append({
                    "model": model, "test_type": "tool", "scenario": tool_name,
                    "tool_calls": [], "prefix_violations": [f"ERROR: {e}"],
                    "reasoning_content_count": 0, "reasoning_content_chars": 0,
                    "reasoning_count": 0, "reasoning_chars": 0,
                    "finish_reason": None, "has_reasoning": False,
                    "raw_snippet": str(e)[:200],
                })
            time.sleep(2)
    
    # Summary
    print("\n" + "=" * 70)
    print("SUMMARY")
    print("=" * 70)
    
    # Reasoning summary
    print("\n--- REASONING EMISSION (VPS) ---")
    for model in MODELS:
        for scenario in REASONING_SCENARIOS:
            matches = [r for r in all_results if r["model"] == model and r["test_type"] == "reasoning" and r["scenario"] == scenario["name"]]
            if matches:
                r = matches[0]
                status = "✅ HAS_REASONING" if r["has_reasoning"] else "❌ NO_REASONING"
                print(f"  {model:25} | {scenario['name']:20} | {status} | content_deltas={r['reasoning_content_count']} chars={r['reasoning_content_chars']} | reasoning_deltas={r['reasoning_count']} chars={r['reasoning_chars']}")
    
    # Tool summary
    print("\n--- TOOL EMISSION (VPS) ---")
    for model in MODELS:
        for tool_name in TOOL_SCENARIOS.keys():
            matches = [r for r in all_results if r["model"] == model and r["test_type"] == "tool" and r["scenario"] == tool_name]
            if matches:
                r = matches[0]
                tc_str = ", ".join(r["tool_calls"]) if r["tool_calls"] else "none"
                viol = r["prefix_violations"]
                if viol:
                    status = f"❌ PREFIX_VIOLATION ({viol})"
                elif r["tool_calls"]:
                    status = "✅ PASS"
                else:
                    status = "⚠ NO_TOOL"
                print(f"  {model:25} | {tool_name:6} | {status:25} | emitted: [{tc_str}]")
    
    # Save
    with open("verify_vps_results.json", "w") as f:
        json.dump(all_results, f, indent=2)
    print("\nDetailed results saved to verify_vps_results.json")
    
    # Exit code
    fail_count = sum(1 for r in all_results if r["prefix_violations"])
    reasoning_missing = sum(1 for r in all_results if r["test_type"] == "reasoning" and not r["has_reasoning"])
    sys.exit(1 if fail_count > 0 else 0)

if __name__ == "__main__":
    main()