import json
import os
import subprocess
import sys
import time

EXE = r"C:\Users\EDN\AppData\Roaming\npm\node_modules\@opencode\cli\bin\opencode.exe"
AG = r"C:\Users\EDN\Documents\AION\project\agentgate\target\release\agentgate.exe"

TOOLS = [
    {
        "name": "get_weather",
        "description": "Get weather",
        "parameters": {
            "type": "object",
            "properties": {"location": {"type": "string"}},
            "required": ["location"],
        },
    }
]


def run(command, label):
    cfg = {
        "mcpServers": {
            "agentgate": {
                "type": "command",
                "command": command,
                "args": [
                    "mcp-shim",
                    "--session", "manual-session",
                    "--token", "manual-token",
                    "--host", "127.0.0.1",
                    "--port", "8787",
                ],
                "env": {"AGENTGATE_SHIM_TOOLS": json.dumps(TOOLS)},
            }
        },
        "tools": {"bash": False, "edit": False, "write": False, "read": False},
    }
    env = dict(os.environ)
    env["OPENCODE_CONFIG_CONTENT"] = json.dumps(cfg)
    print(f"\n===== {label}: command={command} =====", flush=True)
    started = time.time()
    proc = subprocess.Popen(
        [EXE, "run", "--model", "opencode/beam-free", "--format", "json", "Say hello"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    try:
        out, err = proc.communicate(timeout=75)
        print(f"elapsed={time.time()-started:.1f}s exit={proc.returncode}")
        print("STDOUT:")
        print(out.decode("utf-8", "replace"))
        print("STDERR:")
        print(err.decode("utf-8", "replace")[:4000])
    except subprocess.TimeoutExpired:
        proc.kill()
        out, err = proc.communicate()
        print(f"TIMEOUT after {time.time()-started:.1f}s")
        print("STDOUT:", out.decode("utf-8", "replace")[:4000])
        print("STDERR:", err.decode("utf-8", "replace")[:4000])


if __name__ == "__main__":
    which = sys.argv[1] if len(sys.argv) > 1 else "both"
    if which in ("bare", "both"):
        run("agentgate", "bare command")
    if which in ("abs", "both"):
        run(AG, "absolute path")
