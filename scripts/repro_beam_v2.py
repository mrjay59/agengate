#!/usr/bin/env python3
"""
REPRO-BEAM V2: Test with tools to trigger bridge mode (mode=bridge tools=91)
This should trigger opencode.json injection and potentially provider.free-limit
"""
import json, time, requests
from datetime import datetime

URL = "http://127.0.0.1:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/opencode/beam-free"

# Simulate AionUI-like tool list (many tools to trigger bridge mode)
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

def call_with_tools(model: str, messages: list, tools: list, stream: bool, attempt: int):
    payload = {
        "model": model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "stream": stream,
        "temperature": 0.1,
        "max_tokens": 1000,
    }
    start = time.time()
    try:
        r = requests.post(URL, headers=AUTH, json=payload, timeout=60, stream=stream)
    except Exception as e:
        return {
            "attempt": attempt, "stream": stream,
            "error": str(e), "timestamp": datetime.now().isoformat(),
        }
    elapsed = time.time() - start
    
    result = {
        "attempt": attempt, "stream": stream,
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

def run_repro():
    messages = [{"role": "user", "content": "List all files in current directory"}]
    all_results = []
    
    print("=" * 70)
    print(f"REPRO-BEAM V2: {MODEL} with tools (bridge mode)")
    print(f"Tools count: {len(TOOLS)}")
    print("=" * 70)
    
    print("\n--- NON-STREAM with tools ---")
    for i in range(3):
        print(f"\nAttempt {i+1} (non-stream, tools)...")
        res = call_with_tools(MODEL, messages, TOOLS, stream=False, attempt=i+1)
        all_results.append(res)
        print(f"  HTTP: {res['http_status']} | Time: {res['elapsed_sec']}s")
        if "json_error" in res:
            print(f"  ERROR: {json.dumps(res['json_error'], indent=2)}")
        if res['http_status'] != 200:
            print(f"  BODY: {res['raw_body'][:1000]}")
        time.sleep(3)
    
    print("\n--- STREAM with tools ---")
    for i in range(3):
        print(f"\nAttempt {i+1} (stream, tools)...")
        res = call_with_tools(MODEL, messages, TOOLS, stream=True, attempt=i+1)
        all_results.append(res)
        print(f"  HTTP: {res['http_status']} | Chunks: {res['chunk_count']} | Time: {res['elapsed_sec']}s")
        if "stream_error" in res:
            print(f"  STREAM ERROR: {json.dumps(res['stream_error'], indent=2)}")
        if res['http_status'] != 200:
            print(f"  First chunks: {res['raw_chunks'][:5]}")
        time.sleep(3)
    
    # Also test chat-only (no tools) for comparison
    print("\n--- CHAT-ONLY (no tools) for comparison ---")
    for i in range(2):
        print(f"\nAttempt {i+1} (chat-only)...")
        res = call_with_tools(MODEL, [{"role": "user", "content": "Say hello"}], [], stream=False, attempt=i+1)
        all_results.append(res)
        print(f"  HTTP: {res['http_status']} | Time: {res['elapsed_sec']}s")
        if "json_error" in res:
            print(f"  ERROR: {json.dumps(res['json_error'], indent=2)}")
        time.sleep(2)
    
    # Summary
    print("\n" + "=" * 70)
    print("SUMMARY")
    print("=" * 70)
    for r in all_results:
        mode = "stream" if r["stream"] else "non-stream"
        status = "OK" if r["http_status"] == 200 and "json_error" not in r and "stream_error" not in r else "ERROR"
        err = r.get("json_error") or r.get("stream_error") or ""
        print(f"  Attempt {r['attempt']} ({mode}): HTTP {r['http_status']} | {status} {err}")
    
    filename = f"repro_beam_v2_{datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
    with open(filename, "w") as f:
        json.dump(all_results, f, indent=2)
    print(f"\nSaved to {filename}")
    
    return all_results

if __name__ == "__main__":
    run_repro()