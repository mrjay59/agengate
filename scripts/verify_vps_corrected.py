#!/usr/bin/env python3
"""
VERIFY-VPS CORRECTED
Fixes:
1. Tool-call SSE parsing: name/id/type only on first chunk per index; later chunks may only have arguments.
   Accumulate per index, then emit once.
2. Reasoning detection now also looks for reasoning_content in non-delta message (non-stream).
3. Uses prompts known to trigger thinking/tool usage from prior sessions.
"""
import json, time, requests
from typing import Dict, List, Tuple

VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODELS = ["agg/9router/ag-poli", "agg/9router/free-forever"]

SCENARIOS = [
    ("reasoning_simple", [{"role":"user","content":"Think step by step: what is 2+2? Show your reasoning."}], False),
    ("reasoning_complex", [{"role":"user","content":"If all Bloops are Razzies and all Razzies are Lazzies, are all Bloops Lazzies? Explain your thinking."}], False),
    ("tool_read",     [{"role":"user","content":"Read the file /etc/hostname and show its contents"}], True),
    ("tool_glob",     [{"role":"user","content":"List all Cargo.toml files using glob pattern **/Cargo.toml"}], True),
    ("tool_grep",     [{"role":"user","content":"Search for 'fn main' in src directory using grep"}], True),
    ("tool_shell",    [{"role":"user","content":"Run shell command: echo hello"}], True),
]

FORBIDDEN_PREFIXES = ["agentgate_", "agentgate__", "mcp__", "opencode_"]

def call_model(model: str, messages: List[Dict]) -> Tuple[Dict, str]:
    payload = {
        "model": model, "messages": messages,
        "stream": True, "temperature": 0.1, "max_tokens": 4000,
    }
    r = requests.post(VPS, headers=AUTH, json=payload, timeout=90, stream=True)
    
    tool_call_map = {}
    content_chars = []
    reasoning_content_chars = []
    reasoning_chars = []
    finish_reason = None
    
    for line in r.iter_lines():
        if not line:
            continue
        sline = line.decode('utf-8')
        if not sline.startswith("data: "):
            continue
        data = sline[6:]
        if data == "[DONE]":
            break
        try:
            chunk = json.loads(data)
        except:
            continue
        choices = chunk.get("choices", [])
        if not choices:
            continue
        delta = choices[0].get("delta", {})
        
        if "content" in delta and delta["content"]:
            content_chars.append(delta["content"])
        
        if "reasoning_content" in delta and delta["reasoning_content"]:
            reasoning_content_chars.append(delta["reasoning_content"])
        if "reasoning" in delta and delta["reasoning"]:
            reasoning_chars.append(delta["reasoning"])
        
        if "tool_calls" in delta:
            for tc in delta["tool_calls"]:
                idx = tc.get("index", 0)
                if idx not in tool_call_map:
                    tool_call_map[idx] = {}
                entry = tool_call_map[idx]
                if "id" in tc:
                    entry["id"] = tc["id"]
                if "type" in tc:
                    entry["type"] = tc["type"]
                fn = tc.get("function", {})
                if "name" in fn:
                    entry["name"] = fn["name"]
                if "arguments" in fn:
                    entry["arguments"] = entry.get("arguments", "") + fn["arguments"]
        
        fr = choices[0].get("finish_reason")
        if fr:
            finish_reason = fr
    
    emitted_tools = []
    for idx in sorted(tool_call_map.keys()):
        entry = tool_call_map[idx]
        if "name" in entry:
            emitted_tools.append(entry["name"])
    
    result = {
        "emitted_tools": emitted_tools,
        "finish_reason": finish_reason,
        "has_content": bool(content_chars),
        "has_reasoning_content": len(reasoning_content_chars) > 0,
        "has_reasoning": len(reasoning_chars) > 0,
        "reasoning_content_total": sum(len(x) for x in reasoning_content_chars),
        "reasoning_total": sum(len(x) for x in reasoning_chars),
        "reasoning_content_first": reasoning_content_chars[0][:150] if reasoning_content_chars else "",
        "reasoning_first": reasoning_chars[0][:150] if reasoning_chars else "",
    }
    return result, ""

def run_all():
    print("=" * 70)
    print("VERIFY-VPS CORRECTED — VPS 10.11.1.21:8780 (bypass 9router)")
    print("=" * 70)
    
    rows = []
    for model in MODELS:
        print(f"\n{'='*70}")
        print(f"MODEL: {model}")
        print(f"{'='*70}")
        for name, msgs, expect_tool in SCENARIOS:
            print(f"\n  [{model}] {name}...")
            res, raw = call_model(model, msgs)
            
            tool_status = "PASS" if res["emitted_tools"] else "NO_TOOL"
            if expect_tool and not res["emitted_tools"]:
                tool_status = "NO_TOOL_CALLS"
            elif res["emitted_tools"]:
                violations = []
                for tc in res["emitted_tools"]:
                    for pfx in FORBIDDEN_PREFIXES:
                        if tc.startswith(pfx):
                            violations.append(pfx)
                if violations:
                    tool_status = f"PREFIX_VIOLATION ({violations})"
                else:
                    tool_status = f"PASS (tools={res['emitted_tools']})"
            
            reason_status = "HAS_REASONING" if res["has_reasoning_content"] or res["has_reasoning"] else "NO_REASONING"
            
            print(f"    Finish: {res['finish_reason']} | Tools: {res['emitted_tools']} ({tool_status}) | Reasoning: {reason_status}")
            print(f"      reasoning_content chars: {res['reasoning_content_total']} first: {res['reasoning_content_first'][:80]}...")
            print(f"      reasoning chars: {res['reasoning_total']} first: {res['reasoning_first'][:80]}...")
            
            rows.append({
                "model": model, "scenario": name,
                "tools": res["emitted_tools"], "tool_status": tool_status,
                "reasoning_content": res["has_reasoning_content"],
                "reasoning": res["has_reasoning"],
                "reasoning_content_chars": res["reasoning_content_total"],
                "reasoning_chars": res["reasoning_total"],
                "finish_reason": res["finish_reason"],
            })
            time.sleep(1)
    
    print("\n" + "=" * 70)
    print("SUMMARY MATRIX")
    print("=" * 70)
    print(f"{'Model':<25} {'Scenario':<20} {'Tools':<30} {'Reasoning'}")
    print("-" * 90)
    for r in rows:
        tools_str = ",".join(r["tools"]) if r["tools"] else "none"
        reason_field = "✅reasoning_content" if r["reasoning_content"] else ("✅reasoning" if r["reasoning"] else "❌NONE")
        print(f"{r['model']:<25} {r['scenario']:<20} {tools_str:<30} {reason_field}")
    
    with open("verify_vps_corrected.json", "w") as f:
        json.dump(rows, f, indent=2)
    print("\nSaved to verify_vps_corrected.json")

if __name__ == "__main__":
    run_all()