import json
import os
import subprocess
import time

EXE = r"C:\Users\EDN\AppData\Roaming\npm\node_modules\@opencode\cli\bin\opencode.exe"

for mode in ("stdin_text", "argv"):
    print(f"\n===== {mode} =====", flush=True)
    if mode == "stdin_text":
        args = [EXE, "run", "--model", "opencode/beam-free", "--format", "json"]
        stdin_data = b"Say hello in one short sentence\n"
    else:
        args = [EXE, "run", "--model", "opencode/beam-free", "--format", "json", "Say hello in one short sentence"]
        stdin_data = b""
    started = time.time()
    proc = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        out, err = proc.communicate(input=stdin_data, timeout=30)
        print(f"elapsed={time.time()-started:.1f}s exit={proc.returncode}")
        print(out.decode("utf-8", "replace")[:2000])
        print("STDERR:", err.decode("utf-8", "replace")[:1000])
    except subprocess.TimeoutExpired:
        proc.kill()
        print("TIMEOUT")
