# devenv 2.4.0 recipe (bench/research/devenv.md). Services, readiness probes and per-checkout
# TCP ports are native: the URLs use the *allocated* ports
# (config.processes.<name>.ports.main.value), not the requested base ports, so a second
# checkout in the same host gets its own ports without any benchmark scripting.
{ pkgs, config, ... }:
{
  languages.python = {
    enable = true;
    package = pkgs.python313;
    uv.enable = true;
    # Dependency installation is the scenario's explicit `uv sync --frozen` step.
    uv.sync.enable = false;
  };

  services.postgres = {
    enable = true;
    package = pkgs.postgresql_17;
    listen_addresses = "127.0.0.1";
    port = 55432;
    initdbArgs = [ "--auth=trust" "--encoding=UTF8" "--locale=C" ];
  };

  services.redis = {
    enable = true;
    package = pkgs.redis;
    bind = "127.0.0.1";
    port = 56379;
    # Common durability policy across lanes.
    extraConfig = ''
      appendonly yes
      appendfsync always
    '';
  };

  # The app's role is the cluster's superuser (the OS user that ran initdb, via libpq's default).
  env.DATABASE_URL = "postgresql://127.0.0.1:${toString config.processes.postgres.ports.main.value}/postgres";
  env.REDIS_URL = "redis://127.0.0.1:${toString config.processes.redis.ports.main.value}/0";
}
