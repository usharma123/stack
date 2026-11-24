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

Registry credentials are `STACK_OCI_USERNAME` / `STACK_OCI_PASSWORD`, bound to exactly one
registry origin with `STACK_OCI_REGISTRY`:

```sh
export STACK_OCI_REGISTRY=https://ghcr.io
export STACK_OCI_USERNAME=my-user
export STACK_OCI_PASSWORD="$GHCR_TOKEN"
```

The origin is a scheme, host and port, with nothing after them: `https://ghcr.io` and
`https://ghcr.io:443` are the same origin, while `http://ghcr.io`, `https://ghcr.io:8443`,
`ghcr.io` and `https://ghcr.io/v2` are not accepted as it. The scheme is the one Stack uses for the
reference: HTTPS, or HTTP for loopback registries such as `http://localhost:5000` (`localhost` and
`127.0.0.1` are different origins).

- Credentials are offered only when that exact origin challenges a request for a reference that
  names it. A bundle reference, redirect, upload location or authentication challenge cannot
  move them anywhere else: every other registry is reached anonymously, so public registries
  keep working beside private ones.
- A registry that requires credentials Stack will not send it fails with `oci_unauthorized`.
- Credentials set without `STACK_OCI_REGISTRY`, with a value that is not an origin, or with only
  one of username and password fail closed with `oci_credentials_unbound` when a registry asks
  for authentication; nothing is sent.

A Bearer challenge names a token service (its realm). Stack asks only a token service on the
registry's own origin or one approved in `STACK_OCI_AUTH_REALMS`, a comma-separated list of
origins such as `https://auth.docker.io`, normalized the same way; an entry that is not an origin
is an error. The approved service receives the credentials only when the bound registry sent the
challenge; for any other registry it is asked anonymously. Token requests never follow
redirects, and the token is used only at the registry that was challenged.

HTTPS cannot redirect authentication to HTTP. Plain HTTP is used only for loopback registries,
or elsewhere with exactly `STACK_OCI_PLAIN_HTTP=1`.

[All docs](../README.md)
