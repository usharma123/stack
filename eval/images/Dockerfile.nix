FROM ev-base
# Multi-user Nix without systemd: daemon is started by the entrypoint
RUN curl -fsSL https://install.determinate.systems/nix | sh -s -- install linux --init none --no-confirm --extra-conf "trusted-users = root agent"
ENV PATH=/nix/var/nix/profiles/default/bin:$PATH
COPY nix-entry.sh /usr/local/bin/nix-entry.sh
ENTRYPOINT ["/usr/local/bin/nix-entry.sh"]
CMD ["sleep", "infinity"]
