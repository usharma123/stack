# Port isolation

Keep every checkout on ports of its own, stable until it asks for new ones, whatever other checkouts do.

## Sub-features

Concurrent compiles get distinct ports; each keeps its ports across compiles; a pin on another checkout's port is refused while a pin on the port its own earlier service would get is honored; `compile --reassign-ports` moves every assigned service to a new port; two checkouts run at once; a checkout out of reach keeps its ports; a deleted checkout's ports are released.

## How to get to it (user POV)

`stack compile`, `stack compile --reassign-ports`, `port = N` in a service, `stack up`, `stack gc`, `stack down`.

## Driving it with the native harness

Use the prerequisites in [the skill](../SKILL.md). Run `.cursor/skills/verify-stack-cli/drive.sh 13-port-isolation` (or `pass2`).

JSON from each compile and `up` shows the ports; a pin's refusal is a `port_conflict`. The model behind it is `verify-stack-ports-mcp`.

## Gotchas

The out-of-reach check makes a directory mode 000, which only an unprivileged user cannot read: as root it is skipped with a note.
