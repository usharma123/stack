#!/bin/bash
/nix/var/nix/profiles/default/bin/nix-daemon > /var/log/nix-daemon.log 2>&1 &
for i in $(seq 1 50); do [ -S /nix/var/nix/daemon-socket/socket ] && break; sleep 0.1; done
exec "$@"
