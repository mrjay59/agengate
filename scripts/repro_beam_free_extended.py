#!/usr/bin/env python3
"""
REPRO-BEAM: Extended test to catch transient free usage limit error
Runs multiple rapid attempts to try to trigger quota exhaustion.
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
    messages = [{"role": "user", "content": "Say hello in one sentence."}]
    all_results = []
    error_count = 0
    
    print("=" * 70)
    print(f"REPRO-BEAM EXTENDED: {MODEL} on local gateway")
    print(f"Rapid attempts to catch transient free usage limit error")
    print("=" * 70)
    
    # Rapid fire: 10 non-stream + 10 stream
    print("\n--- RAPID NON-STREAM (10 attempts) ---")
    for i in range(10):
        res = call(MODEL, messages, stream=False, attempt=i+1)
        all_results.append(res)
        if res.get("http_status") != 200 or "json_error" in res:
            error_count += 1
            print(f"  Attempt {i+1}: HTTP {res.get('http_status')} | ERROR: {res.get('json_error') or res.get('raw_body','')[:200]}")
        else:
            print(f"  Attempt {i+1}: HTTP {res['http_status']} OK ({res['elapsed_sec']}s)")
        time.sleep(1)
    
    print("\n--- RAPID STREAM (10 attempts) ---")
    for i in range(10):
        res = call(MODEL, messages, stream=True, attempt=i+1)
        all_results.append(res)
        if res.get("http_status") != 200 or "stream_error" in res:
            error_count += 1
            print(f"  Attempt {i+1}: HTTP {res.get('http_status')} | ERROR: {res.get('stream_error') or res.get('raw_chunks',[''])[0][:200]}")
        else:
            print(f"  Attempt {i+1}: HTTP {res['http_status']} OK ({res['elapsed_sec']}s)")
        time.sleep(1)
    
    # Summary
    print("\n" + "=" * 70)
    print(f"SUMMARY: {len(all_results)} total attempts, {error_count} errors")
    print("=" * 70)
    
    # Save
    filename = f"repro_beam_extended_{datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
    with open(filename, "w") as f:
        json.dump(all_results, f, indent=2)
    print(f"Saved to {filename}")
    
    return error_count > 0

if __name__ == "__main__":
    found_error = run_repro()
    print(f"\nError reproduced: {found_error}")