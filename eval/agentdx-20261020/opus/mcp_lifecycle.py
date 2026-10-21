"""MCP-only lifecycle on fixtures/run-url-repro: stack_up -> stack_exec/stack_run URL -> stack_down.

usage: python3 -I mcp_lifecycle.py <checkout copy of run-url-repro> <out.json>
Asserts the custom service's WEB_URL is the same loopback URL under stack_exec and stack_run.
"""
import json
import subprocess
import sys

d, out = sys.argv[1], sys.argv[2]
p = subprocess.Popen(["stack", "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
i = 0
log = []


def rpc(method, params=None):
    global i
    i += 1
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params or {}}) + "\n")
    p.stdin.flush()
    while True:
        r = json.loads(p.stdout.readline())
        if r.get("id") == i:
            log.append({"method": method, "params": params, "response": r})
            return r


def tool(name, **args):
    return rpc("tools/call", {"name": name, "arguments": {"dir": d, **args}})["result"]["structuredContent"]


rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "lc", "version": "0"}})
p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
up = tool("stack_up", ttl="10m")
ex = tool("stack_exec", command=["sh", "-c", "printf %s \"$WEB_URL\""])
run = tool("stack_run", task="with_dep")
down = tool("stack_down")
exec_url = ex["data"]["stdout"]
run_url = run["data"]["stdout"].strip().removeprefix("WEB_URL=")
verdict = {
    "up_ok": up["ok"], "down_ok": down["ok"] and down["data"]["confirmed"],
    "exec_url": exec_url, "run_url": run_url,
    "exec_loopback": exec_url.startswith("http://127.0.0.1:"), "run_matches_exec": run_url == exec_url,
}
print(json.dumps(verdict, indent=1))
json.dump({"verdict": verdict, "log": log}, open(out, "w"), indent=1)
p.stdin.close()
p.wait(timeout=10)
sys.exit(0 if all(v for k, v in verdict.items() if k not in ("exec_url", "run_url")) else 1)
