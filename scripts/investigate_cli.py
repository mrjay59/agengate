#!/usr/bin/env python3
import subprocess, json, sys

# Test CLI directly with different models
models = ["opencode/beam", "opencode/beam-free", "opencode/iceman-free"]
for model in models:
    print(f"\n=== Testing {model} ===")
    r = subprocess.run(
        ["opencode", "run", "--model", model, "--format", "json", "--thinking", "hi"],
        capture_output=True, text=True, timeout=30
    )
    lines = r.stdout.strip().split('\n')
    for line in lines[-3:]:  # Last few lines
        if line.strip():
            print(line[:200])
    if r.stderr:
        print("STDERR:", r.stderr[:200])
