#!/usr/bin/env python3
"""
REPRO-BEAM: Reproduce agg/opencode/beam-free free usage limit error
Tests streaming and non-streaming, repeated with intervals.
"""
import json, time, requests
from datetime import datetime

URL = "http://127.0.0.1:8780/v1/chat/completions"
AUTH = {"Authorization": "Bearer default_key"}
MODEL = "agg/opencode/beam-free"

def call(model: str, messages: list, stream: bool, attempt: int):
    payload = {
        "model": model,
        "messages": messages,
        "stream": stream,
        "temperature": 0.1,
        "max_tokens": 1000,
    }
    start = time.time()
    r = requests.post(URL, headers=AUTH, json=payload, timeout=60, stream=stream)
    elapsed = time.time() - start
    
    result = {
        "attempt": attempt,
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
        # Check for error in stream
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
    messages = [{"role": "user", "content": "Say hello in one sentence."}]
    all_results = []
    
    print("=" * 70)
    print(f"REPRO-BEAM: {MODEL} on local gateway")
    print(f"URL: {URL}")
    print("=" * 70)
    
    # First: non-stream test
    print("\n--- NON-STREAM TESTS ---")
    for i in range(3):
        print(f"\nAttempt {i+1} (non-stream)...")
        res = call(MODEL, messages, stream=False, attempt=i+1)
        all_results.append(res)
        print(f"  HTTP: {res['http_status']} | Time: {res['elapsed_sec']}s")
        if "json_error" in res:
            print(f"  ERROR: {res['json_error']}")
        if res['http_status'] != 200:
            print(f"  BODY: {res['raw_body'][:500]}")
        time.sleep(5)
    
    # Then: stream tests
    print("\n--- STREAM TESTS ---")
    for i in range(3):
        print(f"\nAttempt {i+1} (stream)...")
        res = call(MODEL, messages, stream=True, attempt=i+1)
        all_results.append(res)
        print(f"  HTTP: {res['http_status']} | Chunks: {res['chunk_count']} | Time: {res['elapsed_sec']}s")
        if "stream_error" in res:
            print(f"  STREAM ERROR: {res['stream_error']}")
        if res['http_status'] != 200:
            print(f"  First chunks: {res['raw_chunks'][:5]}")
        time.sleep(5)
    
    # Summary
    print("\n" + "=" * 70)
    print("SUMMARY")
    print("=" * 70)
    for r in all_results:
        mode = "stream" if r["stream"] else "non-stream"
        status = "OK" if r["http_status"] == 200 and "json_error" not in r and "stream_error" not in r else "ERROR"
        err = r.get("json_error") or r.get("stream_error") or ""
        print(f"  Attempt {r['attempt']} ({mode}): HTTP {r['http_status']} | {status} {err}")
    
    # Save
    filename = f"repro_beam_{datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
    with open(filename, "w") as f:
        json.dump(all_results, f, indent=2)
    print(f"\nSaved to {filename}")
    
    return all_results

if __name__ == "__main__":
    run_repro()