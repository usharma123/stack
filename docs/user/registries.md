# Publishing bundles to OCI registries

Publish a directory containing `bundle.toml` and its supporting files:

```sh
stack publish ./my-bundle oci:ghcr.io/my-org/my-bundle:1.0.0
```

Use your own registry and repository in place of the example. Stack refuses to move an existing tag to different content unless you pass `--force`.

Consume the published bundle in `stack.toml`:

```toml
[[use]]
bundle = "oci:ghcr.io/my-org/my-bundle:1.0.0"
```

Run `stack compile` to resolve and lock it.

## Authentication and transport

Registry credentials: `STACK_OCI_USERNAME` / `STACK_OCI_PASSWORD`. External token-service origins
require explicit approval in `STACK_OCI_AUTH_REALMS`, a comma-separated list such as
`https://auth.docker.io`. Credentials and authorization headers are never forwarded to external upload
origins or authentication redirects. HTTPS cannot redirect authentication to HTTP. Plain HTTP
is used only for loopback registries, or elsewhere with exactly `STACK_OCI_PLAIN_HTTP=1`.

[All docs](../README.md)
