#!/usr/bin/env python3
"""
Comprehensive tooling test for agentgate gateway.
Tests all tools on both local (127.0.0.1:8780) and VPS (10.11.1.21:8780) gateways.
Reports PASS/FAIL/NO_TOOL per tool with evidence.
"""
import requests, json, time, datetime

LOCAL = "http://127.0.0.1:8780/v1/chat/completions"
VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}

# Models with quota available
MODELS = ["agg/9router/ag-poli", "agg/9router/free-forever"]

# Tool definitions: (label, tool_name, prompt, expected_emitted_name)
# Using actual tool names the gateway knows. Expected = title-case per fix 1551bf1.
TOOL_SPECS = [
    ("shell",      "shell",      "run shell command echo hello",         "Shell"),
    ("read",       "read",       "read file /etc/hosts",                 "Read"),
    ("write",      "write",      "create file test.txt with content hi", "Write"),
    ("glob",       "glob",       "find all .txt files",                  "Glob"),
    ("grep",       "grep",       "search for pattern in files",          "Grep"),
    ("edit",       "edit",       "edit file content",                    "Edit"),
    ("list",       "list",       "list directory contents",              "List"),
    ("execute",    "execute",    "execute command",                      "Execute"),
    ("bash",       "bash",       "bash command",                         "Bash"),
    ("cat",        "cat",        "cat file",                             "Cat"),
    ("patch",      "patch",      "apply patch to file",                  "Patch"),
    ("task",       "task",       "run task",                             "Task"),
    ("todowrite",  "todowrite",  "write todo item",                      "Todowrite"),
    ("todoread",   "todoread",   "read todo list",                       "Todoread"),
]

GATEWAYS = [
    ("VPS", VPS),
    ("Local", LOCAL),
]

def test_tool(gateway_name, gateway_url, model, label, tool_name, prompt, expected_name):
    tool_def = {
        "type": "function",
        "function": {
            "name": tool_name,
            "description": f"{tool_name} tool",
            "parameters": {"type": "object", "properties": {}}
        }
    }
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "tools": [tool_def],
        "tool_choice": "auto",
        "stream": False,
        "temperature": 0.1,
        "max_tokens": 1000,
    }
    
    result = {
        "gateway": gateway_name,
        "model": model,
        "label": label,
        "tool_requested": tool_name,
        "expected_name": expected_name,
        "http_status": None,
        "mode_header": None,
        "emitted_name": None,
        "status": "ERROR",
        "raw_snippet": None,
    }
    
    try:
        r = requests.post(gateway_url, headers=AUTH, json=payload, timeout=60)
        result["http_status"] = r.status_code
        result["mode_header"] = r.headers.get("x-agentgate-mode")
        
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
                                break
                    else:
                        result["status"] = "NO_TOOL"
                result["raw_snippet"] = json.dumps(data)[:500]
            except json.JSONDecodeError:
                result["raw_snippet"] = r.text[:500]
        else:
            result["raw_snippet"] = r.text[:500]
            
    except Exception as e:
        result["status"] = f"EXCEPTION: {e}"
    
    return result

def run_tests():
    print("=" * 80)
    print("COMPREHENSIVE TOOLING TEST — agentgate gateway")
    print(f"Started: {datetime.datetime.now().isoformat()}")
    print("=" * 80)
    
    all_results = []
    
    for gateway_name, gateway_url in GATEWAYS:
        print(f"\n{'='*80}")
        print(f"GATEWAY: {gateway_name} ({gateway_url})")
        print(f"{'='*80}")
        
        for model in MODELS:
            print(f"\n--- Model: {model} ---")
            model_results = []
            
            for label, tool_name, prompt, expected_name in TOOL_SPECS:
                print(f"  Testing {label}...", end=" ", flush=True)
                result = test_tool(gateway_name, gateway_url, model, label, tool_name, prompt, expected_name)
                model_results.append(result)
                all_results.append(result)
                
                emitted = result["emitted_name"] or "none"
                status = result["status"]
                match = "✓" if status == "PASS" else ("✗" if status == "FAIL" else ("-" if status == "NO_TOOL" else "!"))
                print(f"emit={emitted:<12} exp={expected_name:<12} {status} [{match}]")
                time.sleep(0.3)
            
            # Model summary per gateway
            pass_count = sum(1 for r in model_results if r["status"] == "PASS")
            fail_count = sum(1 for r in model_results if r["status"] == "FAIL")
            no_tool_count = sum(1 for r in model_results if r["status"] == "NO_TOOL")
            error_count = sum(1 for r in model_results if r["status"] not in ("PASS", "FAIL", "NO_TOOL"))
            print(f"  Model summary: PASS={pass_count} FAIL={fail_count} NO_TOOL={no_tool_count} ERROR={error_count}")
    
    # Final comprehensive table
    print("\n" + "=" * 80)
    print("FINAL RESULTS MATRIX")
    print("=" * 80)
    print(f"{'Gateway':<8} | {'Model':<20} | {'Tool':<12} | {'Req':<12} | {'Emit':<12} | {'Exp':<12} | {'Status'}")
    print("-" * 80)
    
    for r in all_results:
        emitted = r["emitted_name"] or "none"
        print(f"{r['gateway']:<8} | {r['model']:<20} | {r['label']:<12} | {r['tool_requested']:<12} | {emitted:<12} | {r['expected_name']:<12} | {r['status']}")
    
    # Per-tool summary across all gateways/models
    print("\n" + "=" * 80)
    print("PER-TOOL SUMMARY (across all gateways/models)")
    print("=" * 80)
    print(f"{'Tool':<12} | {'Total':>5} | {'PASS':>5} | {'FAIL':>5} | {'NO_TOOL':>7} | {'ERROR':>5} | {'PASS%':>6}")
    print("-" * 80)
    
    by_tool = {}
    for r in all_results:
        key = r["label"]
        if key not in by_tool:
            by_tool[key] = []
        by_tool[key].append(r)
    
    for label in [t[0] for t in TOOL_SPECS]:
        entries = by_tool.get(label, [])
        total = len(entries)
        p = sum(1 for e in entries if e["status"] == "PASS")
        f = sum(1 for e in entries if e["status"] == "FAIL")
        n = sum(1 for e in entries if e["status"] == "NO_TOOL")
        e = sum(1 for e in entries if e["status"] not in ("PASS", "FAIL", "NO_TOOL"))
        pct = f"{p/total*100:.0f}%" if total else "N/A"
        print(f"{label:<12} | {total:>5} | {p:>5} | {f:>5} | {n:>7} | {e:>5} | {pct:>6}")
    
    # Save JSON
    timestamp = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    filename = f"comprehensive_tool_test_{timestamp}.json"
    with open(filename, "w") as f:
        json.dump({
            "timestamp": timestamp,
            "gateways_tested": [g[0] for g in GATEWAYS],
            "models_tested": MODELS,
            "tools_tested": [t[0] for t in TOOL_SPECS],
            "results": all_results
        }, f, indent=2)
    print(f"\nSaved results to {filename}")
    
    return all_results

if __name__ == "__main__":
    run_tests()