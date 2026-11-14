#!/usr/bin/env python3
"""
FIX-FREELIMIT-03: Re-test agg/opencode/beam-free with tools/bridge
Tests both local and VPS, stream & non-stream, after fix deploy.
"""
import json, time, requests
from datetime import datetime

LOCAL = "http://127.0.0.1:8780/v1/chat/completions"
VPS = "http://10.11.1.21:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/opencode/beam-free"

TOOLS = [
    {"type": "function", "function": {"name": "read", "description": "Read file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "write", "description": "Write file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "glob", "description": "Glob files", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "grep", "description": "Grep search", "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "shell", "description": "Shell command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "execute", "description": "Execute command", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "list", "description": "List directory", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "patch", "description": "Patch file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "patch": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "edit", "description": "Edit file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "old_str": {"type": "string"}, "new_str": {"type": "string"}}}}},
    {"type": "function", "function": {"name": "create", "description": "Create file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}}},
]

def test_endpoint(url, label, messages, tools, stream):
    payload = {
        "model": MODEL,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "stream": stream,
        "temperature": 0.1,
        "max_tokens": 500,
    }
    start = time.time()
    try:
        r = requests.post(url, headers=AUTH, json=payload, timeout=30, stream=stream)
    except Exception as e:
        return {"env": label, "stream": stream, "error": str(e), "timestamp": datetime.now().isoformat()}
    elapsed = time.time() - start
    
    result = {
        "env": label,
        "stream": stream,
        "http_status": r.status_code,
        "headers": dict(r.headers),
        "elapsed_sec": round(elapsed, 3),
        "timestamp": datetime.now().isoformat(),
    }
    
    if stream:
        chunks = []
        for line in r.iter_lines():
            if not line:
                continue
            s = line.decode('utf-8')
            chunks.append(s)
            if s == "data: [DONE]":
                break
        result["raw_chunks"] = chunks
        result["chunk_count"] = len(chunks)
        for chunk in chunks:
            if chunk.startswith("data: "):
                try:
                    data = json.loads(chunk[6:])
                    if "error" in data:
                        result["stream_error"] = data["error"]
                        break
                except:
                    pass
    else:
        result["raw_body"] = r.text
        try:
            data = r.json()
            if "error" in data:
                result["json_error"] = data["error"]
        except:
            pass
    
    return result

def run_all():
    messages = [{"role": "user", "content": "List files in current directory"}]
    all_results = []
    
    print("=" * 70)
    print("FIX-FREELIMIT-03: beam-free with tools/bridge (post fix e397f31)")
    print("=" * 70)
    
    for env_name, url in [("local", LOCAL), ("vps", VPS)]:
        print(f"\n{'='*70}")
        print(f"ENV: {env_name.upper()} ({url})")
        print(f"{'='*70}")
        
        for stream in [False, True]:
            mode = "STREAM" if stream else "NON-STREAM"
            print(f"\n--- {mode} + tools ---")
            res = test_endpoint(url, env_name, messages, TOOLS, stream)
            all_results.append(res)
            
            print(f"  HTTP: {res.get('http_status')} | Time: {res.get('elapsed_sec', 'N/A')}s")
            print(f"  x-agentgate-mode: {res.get('headers', {}).get('x-agentgate-mode', 'N/A')}")
            if "json_error" in res:
                print(f"  JSON ERROR: {json.dumps(res['json_error'])}")
            if "stream_error" in res:
                print(f"  STREAM ERROR: {json.dumps(res['stream_error'])}")
            if res.get('http_status') != 200:
                print(f"  BODY: {res.get('raw_body', str(res.get('raw_chunks', [])))[:500]}")
            time.sleep(2)
    
    # Summary
    print("\n" + "=" * 70)
    print("SUMMARY")
    print("=" * 70)
    for r in all_results:
        mode = "stream" if r["stream"] else "non-stream"
        http = r.get("http_status", "N/A")
        mode_header = r.get("headers", {}).get("x-agentgate-mode", "N/A")
        err = r.get("json_error") or r.get("stream_error")
        status = "OK" if http == 200 and not err else "ERROR"
        print(f"  {r['env']:6} | {mode:10} | HTTP {http:3} | mode={mode_header} | {status} {json.dumps(err) if err else ''}")
    
    # Save
    filename = f"fix_freelimit_03_{datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
    with open(filename, "w") as f:
        json.dump(all_results, f, indent=2)
    print(f"\nSaved to {filename}")
    
    return all_results

if __name__ == "__main__":
    run_all()