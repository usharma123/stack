"""Stdio MCP probe for `stack mcp`: discovery, schemas, valid/invalid calls, recovery.

usage: python3 -I mcp_probe.py <checkout dir> <out.json> [stack binary]
Each check records the request, the response and an assertion verdict. Exit 1 if any assertion fails.
"""
import json
import os
import subprocess
import sys
import time

checkout, out_path = sys.argv[1], sys.argv[2]
binary = sys.argv[3] if len(sys.argv) > 3 else "stack"
proc = subprocess.Popen([binary, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, text=True, bufsize=1)
next_id = 0
log = []


def rpc(method, params=None, notify=False, timeout=180):
    global next_id
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        msg["params"] = params
    if not notify:
        next_id += 1
        msg["id"] = next_id
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()
    if notify:
        return None
    start = time.time()
    while True:
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError("server closed stdout; stderr: " + proc.stderr.read()[-2000:])
        resp = json.loads(line)
        if resp.get("id") == msg["id"]:
            resp["_secs"] = round(time.time() - start, 2)
            return resp


results = []


def check(name, request, resp, ok, note=""):
    results.append({"check": name, "pass": bool(ok), "note": note, "request": request,
                    "response": resp})
    print(("PASS " if ok else "FAIL ") + name + (f"  ({note})" if note else ""))


def call(name, args, timeout=180):
    req = {"name": name, "arguments": args}
    return req, rpc("tools/call", req, timeout=timeout)


def sc(resp):
    return (resp.get("result") or {}).get("structuredContent") or {}


init = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                          "clientInfo": {"name": "agentdx-opus-probe", "version": "0"}})
check("initialize", "initialize", init, "result" in init and init["result"].get("serverInfo"),
      json.dumps(init.get("result", {}).get("serverInfo")))
rpc("notifications/initialized", notify=True)
tools = rpc("tools/list", {})
names = sorted(t["name"] for t in tools["result"]["tools"])
expected = {"stack_status", "stack_up", "stack_run", "stack_exec", "stack_restart", "stack_logs",
            "stack_down", "stack_inspect", "stack_compile", "stack_install", "stack_renew", "stack_gc",
            "stack_doctor", "stack_skill"}
check("tools/list has documented tools", "tools/list", names, expected <= set(names),
      f"missing={sorted(expected - set(names))} extra={sorted(set(names) - expected)}")
schemas = {t["name"]: t.get("inputSchema") for t in tools["result"]["tools"]}
check("every tool has object inputSchema with dir", "schemas", schemas,
      all(s and s.get("type") == "object" and "dir" in s.get("properties", {}) for n, s in schemas.items()
          if n != "stack_gc"),
      "tools lacking dir: " + str([n for n, s in schemas.items() if "dir" not in (s or {}).get("properties", {})]))
check("outputSchema advertised", "schemas", None,
      any(t.get("outputSchema") for t in tools["result"]["tools"]),
      "tools with outputSchema: " + str([t["name"] for t in tools["result"]["tools"] if t.get("outputSchema")]))

d = checkout
req, r = call("stack_status", {"dir": d})
check("status structured healthy", req, r, sc(r).get("ok") and sc(r)["data"].get("healthy") is True)
req, r = call("stack_exec", {"dir": d, "command": ["sh", "-c", "echo hi; exit 7"]})
check("exec exit 7 -> ok true, exit_code 7, isError false", req, r,
      sc(r).get("ok") is True and sc(r)["data"]["exit_code"] == 7 and not r["result"].get("isError"))
req, r = call("stack_exec", {"dir": d, "command": "echo hi"})
check("exec string command refused as usage", req, r,
      r["result"].get("isError") and sc(r).get("error", {}).get("code") == "usage",
      sc(r).get("error", {}).get("hint", ""))
req, r = call("stack_exec", {"dir": d, "command": ["true"], "timeout": 5})
check("unknown arg 'timeout' -> usage with accepted names", req, r,
      sc(r).get("error", {}).get("code") == "usage" and "timeout_secs" in json.dumps(sc(r)),
      sc(r).get("error", {}).get("hint", ""))
req, r = call("stack_exec", {"dir": d, "command": ["true"], "timeout_secs": "5"})
check("wrong type timeout_secs -> usage", req, r, sc(r).get("error", {}).get("code") == "usage",
      sc(r).get("error", {}).get("message", ""))
req, r = call("stack_exec", {"dir": d})
check("missing command -> usage", req, r, sc(r).get("error", {}).get("code") == "usage",
      sc(r).get("error", {}).get("message", ""))
req, r = call("stack_exec", {"dir": d, "command": ["true"], "require": ["postgres"], "require_all": True})
check("conflicting require + require_all -> usage", req, r, sc(r).get("error", {}).get("code") == "usage",
      sc(r).get("error", {}).get("message", ""))
req, r = call("stack_exec", {"dir": d, "command": ["true"], "secrets": ["lower-case"]})
check("invalid secret name -> invalid_secret (or usage)", req, r,
      sc(r).get("error", {}).get("code") in ("invalid_secret", "usage"),
      sc(r).get("error", {}).get("code", ""))
req, r = call("stack_exec", {"dir": d, "command": ["sh", "-c", "sleep 20"], "timeout_secs": 1})
check("exec timeout -> timed_out", req, r, sc(r).get("error", {}).get("code") == "timed_out",
      f"secs={r['_secs']}")
req, r = call("stack_exec", {"dir": d, "command": ["sh", "-c", "printf %s \"$API_URL\""]})
exec_api = sc(r).get("data", {}).get("stdout")
check("exec API_URL is loopback", req, r, (exec_api or "").startswith("http://127.0.0.1:"), exec_api)
req, r = call("stack_run", {"dir": d, "task": "test"})
check("run test structured", req, r, sc(r).get("ok") is True and "exit_code" in sc(r).get("data", {}),
      f"exit_code={sc(r).get('data', {}).get('exit_code')}")
req, r = call("stack_run", {"dir": d, "task": "nope"})
check("unknown task -> unknown_task listing tasks", req, r,
      sc(r).get("error", {}).get("code") == "unknown_task", sc(r).get("error", {}).get("hint", ""))
req, r = call("stack_logs", {"dir": d, "service": "api", "tail": 3})
check("logs api", req, r, sc(r).get("ok") and len(sc(r)["data"]["lines"]) <= 3)
req, r = call("stack_logs", {"dir": d, "service": "nosuch"})
check("logs unknown service -> unknown_service", req, r,
      sc(r).get("error", {}).get("code") == "unknown_service", sc(r).get("error", {}).get("hint", ""))
req, r = call("stack_status", {"dir": "/nonexistent/agentdx"})
check("status on missing dir -> structured error", req, r, r["result"].get("isError") and sc(r).get("error", {}).get("code"),
      sc(r).get("error", {}).get("code", ""))
req, r = call("stack_status", {})
check("status without dir", req, r, True,
      f"isError={r.get('result', {}).get('isError')} code={sc(r).get('error', {}).get('code')}")
req, r = call("stack_nonexistent", {"dir": d})
check("unknown tool -> error", req, r, "error" in r or r.get("result", {}).get("isError"), json.dumps(r)[:200])
req, r = call("stack_restart", {"dir": d, "services": ["api"]})
check("restart api", req, r, sc(r).get("ok") is True and sc(r)["data"].get("restarted") == ["api"],
      json.dumps(sc(r).get("error") or sc(r).get("data", {}).get("restarted")))
req, r = call("stack_status", {"dir": d})
check("status after restart healthy (recovery)", req, r, sc(r).get("ok") and sc(r)["data"].get("healthy"))
req, r = call("stack_skill", {"dir": d, "tool": "python"})
check("skill python", req, r, True, json.dumps(sc(r))[:200])
req, r = call("stack_inspect", {"dir": d})
check("inspect", req, r, sc(r).get("ok") is True)
req, r = call("stack_doctor", {"dir": d})
check("doctor", req, r, sc(r).get("ok") is True)
# text content mirrors structured content
r_text = (r.get("result") or {}).get("content", [{}])[0].get("text", "")
try:
    same = json.loads(r_text) == sc(r)
except Exception:
    same = False
check("content[0].text is the same JSON as structuredContent", "doctor", None, same)

proc.stdin.close()
try:
    proc.wait(timeout=10)
except subprocess.TimeoutExpired:
    proc.kill()
check("server exits on stdin EOF", "eof", proc.returncode, proc.returncode is not None, f"rc={proc.returncode}")
json.dump({"tools": tools["result"]["tools"], "results": results}, open(out_path, "w"), indent=1)
fails = [r["check"] for r in results if not r["pass"]]
print(f"{len(results) - len(fails)}/{len(results)} passed; fails: {fails}")
sys.exit(1 if fails else 0)
