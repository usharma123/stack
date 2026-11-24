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

A token service that fails is named by its origin alone: the realm's path and query, which come
from the registry's challenge, and the service's reply are never echoed in errors.

## References, digests and the cache

A reference's repository and tag must follow the registry grammar (lowercase repository parts
separated by `.`, `_`, `__` or `-`; tags of letters, digits, `_`, `.` and `-`, at most 128), and
its registry is a host and optional port, so nothing in it adds a path, query, fragment or
userinfo to a request (`source_invalid` otherwise). Every digest, whether a reference's
(`@sha256:...`), the one `stack.lock` records (`lock_invalid` otherwise), or the layer a manifest
names (`bundle_invalid` otherwise, before the layer is requested), must be `sha256:` followed by
64 lowercase hex digits before it names a cache directory or a registry request.

Pulled bundles are kept in Stack's cache by manifest digest and shared by every project. A
cached bundle is reused only with evidence that it is what that manifest holds:

- `stack.lock` pinning that digest with that content hash (checked without the registry, so
  locked projects work offline);
- or, for a reference not yet locked (or `compile --update`), the record Stack keeps beside each
  bundle it installs (`.oci-<digest>.evidence.json`: the manifest and layer digests, verified when
  the layer was downloaded, and the content hash of what it expanded to), as long as the cached
  tree still has that content hash.

Without either (a cache an earlier Stack filled, or one restored from elsewhere), Stack fetches
the manifest and layer again, verifies them, and reuses the cached tree only if it is the
layer's, recording the evidence so later uses need no registry. A cached tree that no longer
matches is `content_hash_mismatch` and is left in place for you to delete, since other projects
may be using it. A cached tree holding `.git` or `.stack` under any spelling (see
[bundles](bundles.md)) is `bundle_invalid`: delete it and retry. The evidence guards against a
cache Stack did not fill that way, not against another process running as you, which can change
the cache, its evidence or a project's `stack.lock` alike.

[All docs](../README.md)
