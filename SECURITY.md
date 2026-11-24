# Security

## Reporting a vulnerability

Please report a suspected vulnerability privately to the maintainers through the repository's
host, not in a public issue, pull request or discussion. Include the Stack version
(`stack --version`), the platform, and the steps that show the problem. There is no dedicated
security contact, response time or disclosure timeline yet.

## Supported versions

Stack is early (0.x) and has no support or backport policy for older releases.

## What Stack protects, and what it does not

Stack keeps checkouts from reaching each other's services and data by mistake. It is not a
security boundary between users or processes on one machine:

- Services listen on `127.0.0.1` without authentication (Postgres `trust`, Redis without a
  password, CockroachDB `--insecure`, SpiceDB with a development key). Any local process, of any
  user, can connect to them.
- Stack, its supervisors, services, tasks and `stack mcp` run as your user. Anything else
  running as you can read or change Stack's state and call fnox for secrets that were not
  granted. Secret grants control what Stack injects; they are not access control.
- Bundles are code you run: their tasks, setup commands, probes and services run with your
  privileges.
- MCP roots and `dir` choose the checkout a call acts on; they are not a sandbox. Purging
  checkouts selected machine-wide by labels over MCP needs `stack mcp --allow-host-purge`.

The full model, with what each guarantee covers on which platform, is in
[docs/user/guarantees.md](docs/user/guarantees.md#security-model).
