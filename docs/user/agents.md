# JSON output and MCP

Use `--json` for structured output. Stack emits an object on stdout with `ok`, and errors include a stable `code`, `hint`, and `details`. `stack gc --watch --json` emits one object per pass.

```sh
stack --json inspect
stack --json up --ttl 30m
stack --json exec --require-all --timeout 5m -- python --version
stack --json down
```

`exec --json` captures at most 64 KiB from each output stream and exits with the command's exit code, or 124 on timeout. Without `--json`, commands keep the terminal and `--timeout` is unavailable.

Start the stdio MCP server with:

```sh
stack mcp
```

Configure your MCP client to launch `stack` with the argument `mcp`. The server exposes Stack operations through the same structured result contract: `stack_inspect`, `stack_compile`, `stack_install`, `stack_up`, `stack_status`, `stack_exec`, `stack_logs`, `stack_renew`, `stack_down`, `stack_gc` and `stack_doctor`.

MCP execution is bounded on Unix: at most 64 KiB of each output stream is retained, and the
command's process group is terminated on timeout or completion. Detached children cannot keep
output collection waiting for EOF.

Use `--require <service>` or `--require-all` when a command must have verified services. See [guarantees and limits](guarantees.md) for endpoint checks, leases, and cleanup behavior.

[All docs](../README.md)
