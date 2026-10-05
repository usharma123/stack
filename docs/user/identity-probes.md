# Service identity probes

A custom service can prove which instance it is. stack gives each checkout's service a random
token in `STACK_IDENTITY_<NAME>`; the probe asks the live service through the app's own
connection settings and must print exactly that token:

```toml
[services.web]
run = "exec python3 {{bundle_dir}}/server.py"     # serves $STACK_IDENTITY_WEB at /identity

[services.web.identity]
command = "python3 {{bundle_dir}}/probe.py"       # prints what $WEB_PORT/identity returns
timeout = "5s"                                    # default 5s, at most 30s
```

The probe runs with `sh -c` in the project, with the stack's environment minus every
`STACK_IDENTITY_*` variable, so it can only learn the token from the service. Exit status other
than 0, no output, any other output, more than 4 KiB, or the deadline (its whole process group is
killed) all fail verification and withhold the service's endpoints. A server that is healthy but
belongs to another checkout or to nothing stack started cannot pass. Probes, like `run`, are
trusted code from the bundle: they are bounded, not sandboxed. See
[examples/bundles/webid](../../examples/bundles/webid).

[All docs](../README.md)
