{ pkgs, config, ... }: {
  packages = [ ];
  languages.python = { enable = true; package = pkgs.python313; uv.enable = true; };
  services.postgres = {
    enable = true;
    package = pkgs.postgresql_17;
    listen_addresses = "127.0.0.1";
    initialScript = "CREATE ROLE postgres SUPERUSER LOGIN;";
  };
  services.redis.enable = true;
  env.UV_PYTHON_DOWNLOADS = "never";
  env.DATABASE_URL = "postgresql://postgres@127.0.0.1:${toString config.services.postgres.port}/postgres";
  env.REDIS_URL = "redis://127.0.0.1:${toString config.services.redis.port}/0";
}
