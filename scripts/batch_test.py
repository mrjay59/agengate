#!/usr/bin/env python3
"""Run one batch of tool tests, save JSON, print summary."""
import json, sys, time
sys.path.insert(0, "C:/Users/EDN/Documents/AION/project/agentgate/scripts")
import comprehensive_tool_test as t

def batch(env_name, url, model, tools=None):
    if tools is None:
        tools = t.TOOL_SPECS
    results = []
    print(f"=== {env_name} / {model} ===")
    for tk, tn, p, exp in tools:
        r = t.test_tool(env_name, url, model, tk, tn, p, exp)
        results.append(r)
        st = r["status"]
        em = r["emitted_name"] or "none"
        icon = "P" if st == "PASS" else ("F" if st == "FAIL" else "?")
        print(f"{icon} {tk:15} {st:12} emitted={em}")
        time.sleep(0.3)
    p = sum(1 for r in results if r["status"] == "PASS")
    f = sum(1 for r in results if r["status"] == "FAIL")
    n = sum(1 for r in results if r["status"] not in ("PASS", "FAIL"))
    print(f"{env_name} {model}: {p} PASS, {f} FAIL, {n} OTHER")
    return results

if __name__ == "__main__":
    which = sys.argv[1] if len(sys.argv) > 1 else "b1"
    out = f"batch_{which}.json"

    if which == "b1":
        res = batch("VPS", t.VPS, "agg/9router/ag-poli")
    elif which == "b2":
        res = batch("VPS", t.VPS, "agg/9router/free-forever")
    elif which == "b3":
        res = batch("LOCAL", t.LOCAL, "agg/9router/ag-poli")
    elif which == "b4":
        res = batch("LOCAL", t.LOCAL, "agg/9router/free-forever")
    elif which == "b5":
        res = batch("VPS", t.VPS, "agg/opencode/deepseek-v4.1-flash-free")
    else:
        print("usage: batch_test.py b1|b2|b3|b4|b5")
        sys.exit(1)

    with open(out, "w") as f:
        json.dump(res, f, indent=2)
    print(f"saved -> {out}")
