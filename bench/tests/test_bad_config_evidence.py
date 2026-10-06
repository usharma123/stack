"""bad_config evidence: the requested tag alone never proves an intended refusal.

Offline only: no Docker, network or services. RAW holds exact bytes of retained smoke-run
step logs (bench/results is gitignored), with the SHA-256 of each full raw file; when the
local raw file is present the embedded copy is checked against it. devenv-2's 73 KB stderr
is embedded as its exact final 1200 bytes (stderr_tail=True).

python3 -m unittest bench.tests.test_bad_config_evidence -v   (or discover -s bench/tests)
"""
from pathlib import Path
import hashlib
import sys
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

from rwb import verify  # noqa: E402
from rwb.adapters import registry  # noqa: E402
from rwb.adapters.base import FEATURES, Adapter  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.testing import FakeRecorder, FakeTransport, FakeWorld  # noqa: E402

RAW = {
    'tilt-1': dict(tool='tilt', path='smoke-tilt-1/logs/0057-d-start-invalid',
        stdout_sha256='ecde0f0107d3c527955cef2e7887ec362c81ad45eb959cd26abb02b3bcbca598', stdout_tail=False,
        stdout='Tilt started (without browser UI)\nv0.37.8, built 2026-10-01\nTilt analytics disabled: Environment variable TILT_DISABLE_ANALYTICS=1\n\nInitial Build\nLoading Tiltfile at: /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/d/Tiltfile\nSuccessfully loaded Tiltfile (28.506166ms)\n     postgres │ \n     postgres │ Initial Build\n     postgres │ STEP 1/1 — Deploying\n        redis │ \n        redis │ Initial Build\n        redis │ STEP 1/1 — Deploying\n     postgres │  Image postgres:99.99.99-alpine Pulling \n     postgres │ error getting credentials - err: exit status 1, out: ``\n     postgres │ \n     postgres │ got unexpected error during build/deploy: command ["/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/tools/docker-compose" "--project-name" "rwb-20261006t161123-8af6e8-d" "--project-directory" "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/d" "-f" "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/d/compose.yaml" "up" "--no-deps" "--remove-orphans" "--no-build" "-d" "postgres" "--wait"] failed.\n     postgres │ error: exit status 1\n     postgres │ \n     postgres │ ERROR: Build Failed: command ["/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/tools/docker-compose" "--project-name" "rwb-20261006t161123-8af6e8-d" "--project-directory" "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/d" "-f" "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/d/compose.yaml" "up" "--no-deps" "--remove-orphans" "--no-build" "-d" "postgres" "--wait"] failed.\n     postgres │ error: exit status 1\n',
        stderr_sha256='5e906cb34389be32056faf8a8d4214db9aa5688cbcbabf69bd08f8df6afe6166', stderr_tail=False,
        stderr='Error: command ["/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/tools/docker-compose" "--project-name" "rwb-20261006t161123-8af6e8-d" "--project-directory" "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/d" "-f" "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj/w/d/compose.yaml" "up" "--no-deps" "--remove-orphans" "--no-build" "-d" "postgres" "--wait"] failed.\nerror: exit status 1\n\n',
    ),
    'vagrant-1': dict(tool='vagrant', path='smoke-vagrant-1/logs/0054-d-start-invalid',
        stdout_sha256='6dc9bd63ece263600d9b770c8d47fa848be1f60947a6a014ff76bf3ccd0a8544', stdout_tail=False,
        stdout="Bringing machine 'pg' up with 'docker' provider...\nBringing machine 'redis' up with 'docker' provider...\nBringing machine 'app' up with 'docker' provider...\n==> pg: Creating and configuring docker networks...\n==> pg: Creating the container...\n    pg:   Name: rwb-20261006t161521-0018bf-d-pg\n    pg:  Image: postgres:99.99.99-alpine\n",
        stderr_sha256='3eaf82d4773650c17af4ee092b942631ffca6318fa7e7917fee045e417ecac4d', stderr_tail=False,
        stderr='A Docker command executed by Vagrant didn\'t complete successfully!\nThe command run along with the output from the command is shown\nbelow.\n\nCommand: ["docker", "run", "--name", "rwb-20261006t161521-0018bf-d-pg", "-d", "-e", "POSTGRES_USER=postgres", "-e", "POSTGRES_PASSWORD=bench", "-e", "POSTGRES_DB=postgres", "--label", "rwb.run=20261006t161521-0018bf", "--label", "rwb.instance=rwb-20261006t161521-0018bf-d", "--label", "rwb.service=1", "--network", "rwb-20261006t161521-0018bf-d-net", "--network-alias", "pg", "--mount", "type=volume,source=rwb-20261006t161521-0018bf-d-pgdata,target=/var/lib/postgresql/data", "postgres:99.99.99-alpine", {:notify=>[:stdout, :stderr]}]\n\nStderr: Unable to find image \'postgres:99.99.99-alpine\' locally\ndocker: error getting credentials - err: exit status 1, out: ``\n\nRun \'docker run --help\' for more information\n\n\nStdout: \n',
    ),
    'devpod-1': dict(tool='devpod', path='smoke-devpod-1/logs/0061-d-start-invalid',
        stdout_sha256='92efba92acee389cbfb080bdab1d7eac920d1f54f296f969b237ee3ecfc7b39f', stdout_tail=False,
        stdout='\x1b[0;1;37m11:46:27 \x1b[0m\x1b[0;1;36minfo \x1b[0mCreating devcontainer...\n\x1b[0;1;37m11:46:28 \x1b[0m\x1b[0;1;36minfo \x1b[0mdevcontainer up: start container: build and extend docker-compose: inspect image python:3.13.99-slim-bookworm: get image config remotely: retrieve image python:3.13.99-slim-bookworm: error getting credentials - err: exit status 1, out: ``\n',
        stderr_sha256='d1d00eb2081ff9994c5241d121cfc2b46c6472685b354b36d49a77d71109d8d1', stderr_tail=False,
        stderr='\x1b[0;1;37m11:46:28 \x1b[0m\x1b[0;1;31mfatal \x1b[0mrun agent command: Process exited with status 1\n',
    ),
    'berth-1': dict(tool='berth', path='smoke-berth-1/logs/0053-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='31411499c8103a3c93c23068c5d83f6d4d6ffd89134deb1d2c0ec7cf911ec7e8', stderr_tail=False,
        stderr='✗ postgres Pulling \nerror getting credentials - err: exit status 1, out: ``\n',
    ),
    'branchbox-1': dict(tool='branchbox', path='smoke-branchbox-1/logs/0053-d-start-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='484abbfb82e84628a8029aafc8285d485b0913956cbb34f2ae5d8a3941c0f4cd', stderr_tail=False,
        stderr='2026-10-06T16:10:53.317459Z  INFO worktree_core::devcontainer_runtime::runtime: Starting devcontainer with Docker Compose compose_files=["/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161017-b3e6f5-5pbfgqh2/w/rwb-20261006t161017b3e6f5-d/.devcontainer/compose.yaml", "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/branchbox-devcontainer-environment-6k75gv.yaml"] service=app project=rwb-20261006t161017b3e6f5-d\nError: Failed to start devcontainer\n\nCaused by:\n    docker compose up failed:  postgres Pulling \n    error getting credentials - err: exit status 1, out: ``\n    \n',
    ),
    'compose-2': dict(tool='compose', path='smoke-compose-2/logs/0058-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='6ff26ad949fbba6fd1f53cfa066d6473fc38930a3ad00a4ffade483d76fa8712', stderr_tail=False,
        stderr='failed to resolve digest for docker.io/library/postgres:99.99.99-alpine: Error response from daemon: manifest unknown: manifest unknown\n',
    ),
    'devbox-1': dict(tool='devbox', path='smoke-devbox-1/logs/0057-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='83aaaaa8501295360f886231f0085694270bc119d6a19e59049db6edbf7a7290', stderr_tail=False,
        stderr='Info: Ensuring packages are installed.\nError: postgresql@99.99.99: package not found\n\n',
    ),
    'devcontainers-2': dict(tool='devcontainers', path='smoke-devcontainers-2/logs/0059-d-setup-invalid',
        stdout_sha256='55b92b7b7c1d75d8f0cc8dc33c462e052ee7c74fffa3d9699920d4544fdfb6b7', stdout_tail=False,
        stdout='{"outcome":"error","message":"Command failed: docker pull python:3.13.99-slim-bookworm","description":"An error occurred building the container."}\n',
        stderr_sha256='d7e95e085f2859452a591ea49fd95caead35dcf490106dcb4ac700e1afb1732d', stderr_tail=False,
        stderr='[2026-10-06T15:44:26.286Z] @devcontainers/cli 0.89.0. Node.js v24.16.0. darwin 24.6.0 arm64.\n[2026-10-06T15:44:26.449Z] Start: Run: docker compose -f /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t154310-ba3a3f-o52zk4k4/w/d/.devcontainer/compose.yaml --profile * config\n[2026-10-06T15:44:26.510Z] Start: Run: docker compose -f /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t154310-ba3a3f-o52zk4k4/w/d/.devcontainer/compose.yaml --profile * config\n[2026-10-06T15:44:26.582Z] name: devcontainer\nservices:\n  app:\n    build:\n      context: /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t154310-ba3a3f-o52zk4k4/w/d/.devcontainer\n      dockerfile: Dockerfile\n    command:\n      - sleep\n      - infinity\n    depends_on:\n      postgres:\n        condition: service_healthy\n        required: true\n      redis:\n        condition: service_healthy\n        required: true\n    environment:\n      DATABASE_URL: postgresql://bench:bench@postgres:5432/bench\n      REDIS_URL: redis://redis:6379/0\n    init: true\n    networks:\n      default: null\n    volumes:\n      - type: bind\n        source: /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t154310-ba3a3f-o52zk4k4/w/d\n        target: /workspace\n  postgres:\n    environment:\n      POSTGRES_DB: bench\n      POSTGRES_PASSWORD: bench\n      POSTGRES_USER: bench\n    healthcheck:\n      test:\n        - CMD\n        - pg_isready\n        - -h\n        - 127.0.0.1\n        - -U\n        - bench\n        - -d\n        - bench\n      timeout: 3s\n      interval: 1s\n      retries: 60\n    image: postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94\n    networks:\n      default: null\n    volumes:\n      - type: volume\n        source: pgdata\n        target: /var/lib/postgresql/data\n        volume: {}\n  redis:\n    command:\n      - redis-server\n      - --appendonly\n      - "yes"\n      - --appendfsync\n      - always\n    healthcheck:\n      test:\n        - CMD\n        - redis-cli\n        - ping\n      timeout: 3s\n      interval: 1s\n      retries: 60\n    image: redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0\n    networks:\n      default: null\n    volumes:\n      - type: volume\n        source: redisdata\n        target: /data\n        volume: {}\nnetworks:\n  default:\n    name: devcontainer_default\nvolumes:\n  pgdata:\n    name: devcontainer_pgdata\n  redisdata:\n    name: devcontainer_redisdata\n[2026-10-06T15:44:26.584Z] Start: Run: docker inspect --type image python:3.13.99-slim-bookworm\n[2026-10-06T15:44:26.821Z] Start: Run: docker-credential-desktop get\n[2026-10-06T15:44:27.684Z] Error fetching image details: No manifest found for docker.io/library/python:3.13.99-slim-bookworm.\n[2026-10-06T15:44:27.684Z] Start: Run: docker pull python:3.13.99-slim-bookworm\n[2026-10-06T15:44:28.420Z] Error response from daemon: failed to resolve reference "docker.io/library/python:3.13.99-slim-bookworm": docker.io/library/python:3.13.99-slim-bookworm: not found\n[2026-10-06T15:44:28.421Z] Retrying (Attempt 0) with error \n\t\t\t  \'Command failed: docker pull python:3.13.99-slim-bookworm\r\n\'\n[2026-10-06T15:44:29.422Z] Start: Run: docker pull python:3.13.99-slim-bookworm\n[2026-10-06T15:44:29.808Z] Error response from daemon: failed to resolve reference "docker.io/library/python:3.13.99-slim-bookworm": docker.io/library/python:3.13.99-slim-bookworm: not found\n[2026-10-06T15:44:29.810Z] Retrying (Attempt 1) with error \n\t\t\t  \'Command failed: docker pull python:3.13.99-slim-bookworm\r\n\'\n[2026-10-06T15:44:30.811Z] Start: Run: docker pull python:3.13.99-slim-bookworm\n[2026-10-06T15:44:30.972Z] Error response from daemon: failed to resolve reference "docker.io/library/python:3.13.99-slim-bookworm": docker.io/library/python:3.13.99-slim-bookworm: not found\n[2026-10-06T15:44:30.973Z] Retrying (Attempt 2) with error \n\t\t\t  \'Command failed: docker pull python:3.13.99-slim-bookworm\r\n\'\n[2026-10-06T15:44:31.975Z] Start: Run: docker pull python:3.13.99-slim-bookworm\n[2026-10-06T15:44:32.152Z] Error response from daemon: failed to resolve reference "docker.io/library/python:3.13.99-slim-bookworm": docker.io/library/python:3.13.99-slim-bookworm: not found\n[2026-10-06T15:44:32.154Z] Retrying (Attempt 3) with error \n\t\t\t  \'Command failed: docker pull python:3.13.99-slim-bookworm\r\n\'\n[2026-10-06T15:44:33.155Z] Start: Run: docker pull python:3.13.99-slim-bookworm\n[2026-10-06T15:44:33.322Z] Error response from daemon: failed to resolve reference "docker.io/library/python:3.13.99-slim-bookworm": docker.io/library/python:3.13.99-slim-bookworm: not found\n[2026-10-06T15:44:33.323Z] Retrying (Attempt 4) with error \n\t\t\t  \'Command failed: docker pull python:3.13.99-slim-bookworm\r\n\'\n[2026-10-06T15:44:34.325Z] Command failed: docker inspect --type image python:3.13.99-slim-bookworm\n[2026-10-06T15:44:34.325Z] []\n[2026-10-06T15:44:34.325Z] Error response from daemon: No such image: python:3.13.99-slim-bookworm\r\n\n[2026-10-06T15:44:34.325Z] Command failed: docker pull python:3.13.99-slim-bookworm\n',
    ),
    'devenv-2': dict(tool='devenv', path='smoke-devenv-2/logs/0060-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='526944758cb5ceecbc78727a6b1cce85458cc17dc48834c86719a82a31b9f31e', stderr_tail=True,
        stderr='ix:1312:5\x1b[0m:\n  \x1b[31m \x1b[0m   1311|     def:\n  \x1b[31m \x1b[0m   1312|     if def._type or "" == "merge" then\n  \x1b[31m \x1b[0m       |     \x1b[31;1m^\x1b[0m\n  \x1b[31m \x1b[0m   1313|       concatMap dischargeProperties def.contents\n  \x1b[31m \x1b[0m\n  \x1b[31m \x1b[0m\n  \x1b[31m \x1b[0m … while evaluating the attribute \'\x1b[35;1mvalue\x1b[0m\'\n  \x1b[31m \x1b[0m   \x1b[34;1mat \x1b[35;1m/nix/store/98mad1s6hrryyflb6qf6hd4xyb8r7cy4-source/lib/modules.nix:805:21\x1b[0m:\n  \x1b[31m \x1b[0m    804|             inherit (module) file;\n  \x1b[31m \x1b[0m    805|             inherit value;\n  \x1b[31m \x1b[0m       |                     \x1b[31;1m^\x1b[0m\n  \x1b[31m \x1b[0m    806|           }) module.config\n  \x1b[31m \x1b[0m\n  \x1b[31m \x1b[0m\n  \x1b[31m \x1b[0m\n  \x1b[31m \x1b[0m \x1b[34;1mat \x1b[35;1m/home/agent/rwb/d/devenv.nix:17:15\x1b[0m:\n  \x1b[31m \x1b[0m     16|     enable = true;\n  \x1b[31m \x1b[0m     17|     package = pkgs.postgresql_99;\n  \x1b[31m \x1b[0m       |               \x1b[31;1m^\x1b[0m\n  \x1b[31m \x1b[0m     18|     listen_addresses = "127.0.0.1";\n  \x1b[31m \x1b[0m\n  \x1b[31m \x1b[0m \x1b[31;1merror:\x1b[0m attribute \'\x1b[35;1mpostgresql_99\x1b[0m\' missing\n  \x1b[31m \x1b[0m\n  \x1b[31m \x1b[0m Did you mean one of \x1b[35;1mpostgresql_13\x1b[0m, \x1b[35;1mpostgresql_14\x1b[0m, \x1b[35;1mpostgresql_15\x1b[0m, \x1b[35;1mpostgresql_16\x1b[0m or \x1b[35;1mpostgresql_17\x1b[0m?\n\n',
    ),
    'dnvr-1': dict(tool='dnvr', path='smoke-dnvr-1/logs/0055-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='732383419ed3cac6081fe032c231c13f791cf7b34e9504b38b89f265e4d9c680', stderr_tail=False,
        stderr='warning: creating lock file "/home/agent/rwb/d/dnvr/flake.lock": \n• Added input \'dnvr\':\n    \'github:dialohq/dnvr/a66c2bb\' (2026-09-08)\n• Added input \'dnvr/nixpkgs\':\n    follows \'nixpkgs\'\n• Added input \'nixpkgs\':\n    \'github:NixOS/nixpkgs/151fa4e\' (2026-10-06)\nerror:\n       … while calling the \'derivationStrict\' builtin\n         at «nix-internal»/derivation-internal.nix:38:12:\n           37|\n           38|   strict = drvFunc drvAttrs;\n             |            ^\n           39|\n\n       … while evaluating derivation \'dnvr-rwb\'\n         whose name attribute is located at «github:NixOS/nixpkgs/151fa4e»/pkgs/stdenv/generic/make-derivation.nix:651:11\n\n       … while evaluating attribute \'nativeBuildInputs\' of derivation \'dnvr-rwb\'\n         at «github:NixOS/nixpkgs/151fa4e»/pkgs/stdenv/generic/make-derivation.nix:719:11:\n          718|           depsBuildBuild = buildBuildOutputs;\n          719|           nativeBuildInputs = buildHostOutputs;\n             |           ^\n          720|           depsBuildTarget = buildTargetOutputs;\n\n       … while evaluating the option `dnvr.shells.rwb.processes.pg.packages\':\n\n       … while evaluating definitions from `/nix/store/x9pq29gd9hmgwrlschm4w91z5yrq4lgp-source/presets/postgres.nix\':\n\n       … while evaluating the option `dnvr.shells.rwb.processes.pg.package\':\n\n       … while evaluating definitions from `<unknown-file>\':\n\n       (stack trace truncated; use \'--show-trace\' to show the full, detailed trace)\n\n       error: attribute \'postgresql_99\' missing\n       at /home/agent/rwb/d/dnvr/flake.nix:39:27:\n           38|                 imports = [ presets.postgres ];\n           39|                 package = pkgs.postgresql_99;\n             |                           ^\n           40|                 database = "postgres";\n       Did you mean one of postgresql_19, postgresql_13, postgresql_14, postgresql_15 or postgresql_16?\n',
    ),
    'flox-1': dict(tool='flox', path='smoke-flox-1/logs/0053-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='07222e09906b0b1df6f706e4847f861b6155493eccd53d11f5ddaa7d6f652443', stderr_tail=False,
        stderr="! You are not logged in to FloxHub. Run 'flox auth login' to log in.\n✘ ERROR: resolution failed: could not find package 'postgresql_99'.\n",
    ),
    'isola-1': dict(tool='isola', path='smoke-isola-1/logs/0057-d-start-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='8d17c6777f3417659ee774df170ab13d6bc3beb48671053b908484fe1660d6ea', stderr_tail=False,
        stderr='Bringing up cache (redis) for d ...\nwarning: accessory database (postgres) for d could not be brought up; continuing without its env: failed to connect to `user=bench database=postgres`:\n\t127.0.0.1:1 (127.0.0.1): dial error: dial tcp 127.0.0.1:1: connect: connection refused\n\t127.0.0.1:1 (127.0.0.1): dial error: dial tcp 127.0.0.1:1: connect: connection refused\nerror: starting d/fixture: not started: accessory "database" could not be brought up (see the warning above)\nerror: 1 service(s) failed to start; see the errors above\n',
    ),
    'nix-1': dict(tool='nix', path='smoke-nix-1/logs/0055-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='b4b997724958c6bb68868b7579afeb60eb72a18bc9c9a813a1d77b3943d448c3', stderr_tail=False,
        stderr='warning: creating lock file "/home/agent/rwb/d/nix/flake.lock": \n• Added input \'nixpkgs\':\n    \'github:NixOS/nixpkgs/151fa4e\' (2026-10-06)\nerror:\n       … while calling the \'derivationStrict\' builtin\n         at «nix-internal»/derivation-internal.nix:38:12:\n           37|\n           38|   strict = drvFunc drvAttrs;\n             |            ^\n           39|\n\n       … while evaluating derivation \'nix-shell\'\n         whose name attribute is located at «github:NixOS/nixpkgs/151fa4e»/pkgs/stdenv/generic/make-derivation.nix:651:11\n\n       … while evaluating attribute \'nativeBuildInputs\' of derivation \'nix-shell\'\n         at «github:NixOS/nixpkgs/151fa4e»/pkgs/stdenv/generic/make-derivation.nix:719:11:\n          718|           depsBuildBuild = buildBuildOutputs;\n          719|           nativeBuildInputs = buildHostOutputs;\n             |           ^\n          720|           depsBuildTarget = buildTargetOutputs;\n\n       (stack trace truncated; use \'--show-trace\' to show the full, detailed trace)\n\n       error: attribute \'postgresql_99\' missing\n       at /home/agent/rwb/d/nix/flake.nix:15:49:\n           14|           default = pkgs.mkShellNoCC {\n           15|             packages = [ pkgs.python313 pkgs.uv pkgs.postgresql_99 pkgs.redis ];\n             |                                                 ^\n           16|             UV_PYTHON = "${pkgs.python313}/bin/python3";\n       Did you mean one of postgresql_19, postgresql_13, postgresql_14, postgresql_15 or postgresql_16?\n',
    ),
    'organist-1': dict(tool='organist', path='smoke-organist-1/logs/0056-d-setup-invalid',
        stdout_sha256='b93070b1ee3ae2e28058283c0dc551c9ad90034e44697786eea8819f79dfc49a', stdout_tail=False,
        stdout='{"dirtyRevision":"3c23ae9a64c3605bbae1a16e07bd3b9d8cf989e7-dirty","fingerprint":"eb6b3668462f3d2cbc330e338fcd9c09654005bf0bea34750bdb5283e4a9e1ee","lastModified":1791303282,"locked":{"dirtyRev":"3c23ae9a64c3605bbae1a16e07bd3b9d8cf989e7-dirty","dirtyShortRev":"3c23ae9-dirty","lastModified":1791303282,"type":"git","url":"file:///home/agent/rwb/d"},"locks":{"nodes":{"flake-compat":{"flake":false,"locked":{"lastModified":1696426674,"narHash":"sha256-kvjfFW7WAETZlt09AgDn1MrtKzP7t90Vf7vypd3OL1U=","owner":"edolstra","repo":"flake-compat","rev":"0f9255e01c2351cc7d116c072cb317785dd33b33","type":"github"},"original":{"owner":"edolstra","repo":"flake-compat","type":"github"}},"flake-utils":{"inputs":{"systems":"systems"},"locked":{"lastModified":1710146030,"narHash":"sha256-SZ5L6eA7HJ/nmkzGG7/ISclqe6oZdOZTNoesiInkXPQ=","owner":"numtide","repo":"flake-utils","rev":"b1d9ab70662946ef0850d488da1c9019f3a9752a","type":"github"},"original":{"owner":"numtide","repo":"flake-utils","type":"github"}},"nixpkgs":{"locked":{"lastModified":1791260994,"narHash":"sha256-Miqqk/ammqnTUxaoCyvtwoPeLbI1ksFyLxi/CazZpWY=","owner":"NixOS","repo":"nixpkgs","rev":"151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4","type":"github"},"original":{"owner":"NixOS","repo":"nixpkgs","rev":"151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4","type":"github"}},"nixpkgs_2":{"locked":{"lastModified":1719075281,"narHash":"sha256-CyyxvOwFf12I91PBWz43iGT1kjsf5oi6ax7CrvaMyAo=","owner":"NixOS","repo":"nixpkgs","rev":"a71e967ef3694799d0c418c98332f7ff4cc5f6af","type":"github"},"original":{"id":"nixpkgs","ref":"nixos-unstable","type":"indirect"}},"organist":{"inputs":{"flake-compat":"flake-compat","flake-utils":"flake-utils","nixpkgs":"nixpkgs_2"},"locked":{"lastModified":1755004808,"narHash":"sha256-ivs3qgkRULIF925fJTEJfH85B4f+tl5e2gSrVJH58MU=","owner":"nickel-lang","repo":"organist","rev":"a7e4e638cade5e7c4f36a129b80d91bf3538088e","type":"github"},"original":{"owner":"nickel-lang","repo":"organist","rev":"a7e4e638cade5e7c4f36a129b80d91bf3538088e","type":"github"}},"root":{"inputs":{"nixpkgs":"nixpkgs","organist":"organist"}},"systems":{"locked":{"lastModified":1681028828,"narHash":"sha256-Vy1rq5AaRuLzOxct8nz4T6wlgyUR7zLU309k9mBC768=","owner":"nix-systems","repo":"default","rev":"da67096a3b9bf56a91d16901293e51ba5b49a27e","type":"github"},"original":{"owner":"nix-systems","repo":"default","type":"github"}}},"root":"root","version":7},"original":{"type":"git","url":"file:///home/agent/rwb/d"},"originalUrl":"git+file:///home/agent/rwb/d","path":"/nix/store/l92b787bq0s77fag74nbdjwnldgvz13w-source","resolved":{"type":"git","url":"file:///home/agent/rwb/d"},"resolvedUrl":"git+file:///home/agent/rwb/d","url":"git+file:///home/agent/rwb/d"}\n',
        stderr_sha256='404d79502a9f8d8436964aeb2d3144383101f70bb118b460578dec1cef1f2024', stderr_tail=False,
        stderr='warning: Git tree \'/home/agent/rwb/d\' has uncommitted changes\nwarning: creating lock file "/home/agent/rwb/d/flake.lock": \n• Added input \'nixpkgs\':\n    \'github:NixOS/nixpkgs/151fa4e\' (2026-10-06)\n• Added input \'organist\':\n    \'github:nickel-lang/organist/a7e4e63\' (2025-08-12)\n• Added input \'organist/flake-compat\':\n    \'github:edolstra/flake-compat/0f9255e\' (2023-10-04)\n• Added input \'organist/flake-utils\':\n    \'github:numtide/flake-utils/b1d9ab7\' (2024-03-11)\n• Added input \'organist/flake-utils/systems\':\n    \'github:nix-systems/default/da67096\' (2023-04-09)\n• Added input \'organist/nixpkgs\':\n    \'github:NixOS/nixpkgs/a71e967\' (2024-06-22)\nwarning: Git tree \'/home/agent/rwb/d\' has uncommitted changes\nbuilding \'/nix/store/is32dinasj818pd7qjqvwhj90p4yb17h-nickel-res.json.drv\'...\nwarning: auto-disabling sandboxing because the prerequisite namespaces are not available and \'sandbox-fallback\' is enabled; use \'--no-sandbox\' or specify \'sandbox = false\' setting to silence this warning\nwarning: Git tree \'/home/agent/rwb/d\' has uncommitted changes\nbuilding \'/nix/store/7j9cdhhx3v21zpzh1dyr1x4ikpb670n2-nickel-res.json.drv\'...\nwarning: auto-disabling sandboxing because the prerequisite namespaces are not available and \'sandbox-fallback\' is enabled; use \'--no-sandbox\' or specify \'sandbox = false\' setting to silence this warning\nerror:\n       … while calling the \'derivationStrict\' builtin\n         at «nix-internal»/derivation-internal.nix:38:12:\n           37|\n           38|   strict = drvFunc drvAttrs;\n             |            ^\n           39|\n\n       … while evaluating derivation \'shell\'\n         whose name attribute is located at «none»:0\n\n       … while evaluating attribute \'nativeBuildInputs\' of derivation \'shell\'\n\n       (stack trace truncated; use \'--show-trace\' to show the full, detailed trace)\n\n       error: Missing input "nixpkgs#postgresql_99"\n',
    ),
    'pixi-2': dict(tool='pixi', path='smoke-pixi-2/logs/0056-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='84549f54da79da6f3667fda3b8384f76d8a8d886e87bb3578cb2d1f904865b2b', stderr_tail=False,
        stderr="Error:   × failed to solve requirements of environment 'default' for platform 'linux-\n  │ aarch64'\n  ├─▶   × failed to solve the environment\n  │   \n  ╰─▶ Cannot solve the request because of: No candidates were found for\n      postgresql 99.99.99.*.\n      \n\n",
    ),
    'pkgx-1': dict(tool='pkgx', path='smoke-pkgx-1/logs/0052-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='633763c40b414ad159b2eab85f258ba206818a910134637543f12ede4f5b2470', stderr_tail=False,
        stderr='Error: ResolveError { pkg: PackageReq { project: "postgresql.org", constraint: Range { raw: "=99.99.99", set: [Single(Semver { components: [99, 99, 99], major: 99, minor: 99, patch: 99, prerelease: [], build: [], raw: "99.99.99" })] } } }\n+astral.sh/uv +pip.pypa.io +python.org=3.13.15 +postgresql.org=99.99.99 +redis.io=8.10.0 +astral.sh/uv=0.12.22\n',
    ),
    'process-compose-1': dict(tool='process-compose', path='smoke-process-compose-1/logs/0059-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='83db191b5a69314498296272451b9d6aa91f737cec25ee22fe9f8457dc17bca2', stderr_tail=False,
        stderr='warning: creating lock file "/home/agent/rwb/d/toolchain/flake.lock": \n• Added input \'nixpkgs\':\n    \'github:NixOS/nixpkgs/151fa4e\' (2026-10-06)\nerror:\n       … while calling the \'derivationStrict\' builtin\n         at «nix-internal»/derivation-internal.nix:38:12:\n           37|\n           38|   strict = drvFunc drvAttrs;\n             |            ^\n           39|\n\n       … while evaluating derivation \'nix-shell\'\n         whose name attribute is located at «github:NixOS/nixpkgs/151fa4e»/pkgs/stdenv/generic/make-derivation.nix:651:11\n\n       … while evaluating attribute \'nativeBuildInputs\' of derivation \'nix-shell\'\n         at «github:NixOS/nixpkgs/151fa4e»/pkgs/stdenv/generic/make-derivation.nix:719:11:\n          718|           depsBuildBuild = buildBuildOutputs;\n          719|           nativeBuildInputs = buildHostOutputs;\n             |           ^\n          720|           depsBuildTarget = buildTargetOutputs;\n\n       (stack trace truncated; use \'--show-trace\' to show the full, detailed trace)\n\n       error: attribute \'postgresql_99\' missing\n       at /home/agent/rwb/d/toolchain/flake.nix:14:49:\n           13|           default = pkgs.mkShellNoCC {\n           14|             packages = [ pkgs.python313 pkgs.uv pkgs.postgresql_99 pkgs.redis pkgs.bash ];\n             |                                                 ^\n           15|             UV_PYTHON = "${pkgs.python313}/bin/python3";\n       Did you mean one of postgresql_19, postgresql_13, postgresql_14, postgresql_15 or postgresql_16?\n',
    ),
    'services-flake-1': dict(tool='services-flake', path='smoke-services-flake-1/logs/0058-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='fbe1381dbf385a1211448b6371687314488ede4c472456a0459324bdd680ecd4', stderr_tail=False,
        stderr='warning: creating lock file "/home/agent/rwb/d/services/flake.lock": \n• Added input \'flake-parts\':\n    \'github:hercules-ci/flake-parts/024633c\' (2026-10-01)\n• Added input \'flake-parts/nixpkgs-lib\':\n    \'github:nix-community/nixpkgs.lib/f7cd230\' (2026-09-27)\n• Added input \'nixpkgs\':\n    \'github:NixOS/nixpkgs/151fa4e\' (2026-10-06)\n• Added input \'process-compose-flake\':\n    \'github:Platonic-Systems/process-compose-flake/464ff68\' (2026-06-21)\n• Added input \'services-flake\':\n    \'github:juspay/services-flake/0ba7183\' (2026-10-05)\nerror:\n       … while calling the \'derivationStrict\' builtin\n         at «nix-internal»/derivation-internal.nix:38:12:\n           37|\n           38|   strict = drvFunc drvAttrs;\n             |            ^\n           39|\n\n       … while evaluating derivation \'nix-shell\'\n         whose name attribute is located at «github:NixOS/nixpkgs/151fa4e»/pkgs/stdenv/generic/make-derivation.nix:651:11\n\n       … while evaluating attribute \'nativeBuildInputs\' of derivation \'nix-shell\'\n         at «github:NixOS/nixpkgs/151fa4e»/pkgs/stdenv/generic/make-derivation.nix:719:11:\n          718|           depsBuildBuild = buildBuildOutputs;\n          719|           nativeBuildInputs = buildHostOutputs;\n             |           ^\n          720|           depsBuildTarget = buildTargetOutputs;\n\n       … while evaluating the option `perSystem.aarch64-linux.process-compose.services.services.postgres.pg.package\':\n\n       … while evaluating definitions from `/nix/store/dkrd4scvahrjnfprg86jx7g7aa5za14y-source/flake.nix, via option perSystem\':\n\n       (stack trace truncated; use \'--show-trace\' to show the full, detailed trace)\n\n       error: attribute \'postgresql_99\' missing\n       at /home/agent/rwb/d/services/flake.nix:38:23:\n           37|             enable = true;\n           38|             package = pkgs.postgresql_99;\n             |                       ^\n           39|             port = local.pg;\n       Did you mean one of postgresql_19, postgresql_13, postgresql_14, postgresql_15 or postgresql_16?\n',
    ),
    'stack-6': dict(tool='stack', path='smoke-stack-6/logs/0060-d-setup-invalid',
        stdout_sha256='17c70405a06a8d7412b7d112947fc4a93ac29203c038f1f3142ec608c59b2328', stdout_tail=False,
        stdout='{"ok":false,"error":{"code":"resolve_failed","message":"1 version request(s) could not be resolved; stack.lock and the provider config were not changed","hint":"fix the tool name or version, or check network access to the tool\'s release source","details":[{"kind":"service","name":"postgres","requested":"99.99.99","code":"resolve_failed","error":"cannot resolve postgres@99.99.99: no release matches"}]}}\n',
        stderr_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stderr_tail=False,
        stderr='',
    ),
    'worktrunk-1': dict(tool='worktrunk', path='smoke-worktrunk-1/logs/0063-d-setup-invalid',
        stdout_sha256='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', stdout_tail=False,
        stdout='',
        stderr_sha256='ce3288535d5631bfe6c60eb976cae8716636adda8a2bdd353b4e282bf3a60f63', stderr_tail=False,
        stderr="USER HOOKS @ /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t160659-989c60-vyhy7k7k/w/tools/wt-user.toml\n↳ (none configured)\n\nPROJECT HOOKS @ /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t160659-989c60-vyhy7k7k/w/rwbt.rwb-20261006t160659-989c60-d/.config/wt.toml\n❯ pre-start deps: (requires approval)\n  uv sync --frozen --no-python-downloads\n❯ pre-remove services: (requires approval)\n  RWB_PG_PORT={{ branch | hash_port }} RWB_REDIS_PORT={{ ('redis-' ~ branch) | hash_port }} docker compose -p {{ branch | sanitize }} -f compose.yaml down --volumes --remove-orphans --timeout 30\nerror: No interpreter found for Python 3.13.99 in virtual environments or search path\n",
    ),
    'workz-1': dict(tool='workz', path='smoke-workz-1/logs/0063-d-setup-invalid',
        stdout_sha256='617577531a7ee678cce44c24cd65812356d684237c270e864407da8d0d593593', stdout_tail=False,
        stdout='{\n  "branch": "rwb-20261006t160603-d11564-d",\n  "copied": [],\n  "installed": null,\n  "isolation": {\n    "compose_project": "rwb_20261006t160603_d11564_d",\n    "db_name": "rwb_20261006t160603_d11564_d",\n    "env_file": ".env.local",\n    "port": 3030,\n    "port_count": 10,\n    "port_end": 3039\n  },\n  "symlinked": [],\n  "warnings": [],\n  "worktree": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t160603-d11564-xqyc6ezc/w/rwbz--rwb-20261006t160603-d11564-d"\n}\n',
        stderr_sha256='660c60c808a955cdf9b865f45990fec0c0504d0c8f30c2f15dc31a9af07ee8dd', stderr_tail=False,
        stderr='error: No interpreter found for Python 3.13.99 in virtual environments or search path\n',
    ),
    'mise-2': dict(tool='mise', path='smoke-mise-2/logs/0056-d-setup-invalid',
        stdout_sha256='8609d56efcc9dc8f89cf45ddf324c84d84604a9cccff1057a4e4c4158967ec43', stdout_tail=False,
        stdout='→ Targeting 1 platform(s) for ~/rwb/d/mise.lock: linux-arm64\n→ Processing 5 tool(s): python@3.13.16, uv@0.12.23, pitchfork@2.29.0, postgres@99.99.99, redis@8.10.2\n✓ Updated 4 platform entries (1 skipped)\n✓ Lockfile written to ~/rwb/d/mise.lock\n',
        stderr_sha256='8071a17d1b7e401f066a42c7f405193c5fcf28ca46ff546e8bd679671a27837e', stderr_tail=False,
        stderr='mise trusted ~/rwb/d\nmise trusted ~/rwb/d\nmise lock            postgres@99.99.99 linux-arm64\nmise downloading artifact for lock-time provenance verification: uv-aarch64-unknown-linux-gnu.tar.gz\nmise lock            redis@8.10.2 linux-arm64\nmise lock            pitchfork@2.29.0 linux-arm64\nmise lock            uv@0.12.23 linux-arm64\nmise lock            python@3.13.16 linux-arm64\nmise lock          ✓ 4 platform entries\nmise ERROR failed to resolve postgres for linux-arm64: failed to solve postgresql for linux-arm64\nmise ERROR Version: 2026.10.3 linux-arm64 (2026-10-05)\nmise ERROR Run with --verbose or MISE_VERBOSE=1 for more information\n',
    ),
}

# Classification each retained log must get from the adapter's own pattern/refusal/prerequisite.
# Tilt/Vagrant/DevPod were recorded pass by the old tag-anywhere gate; their refusal is a Docker
# credential-helper failure before any registry answered. Berth/BranchBox were already blocked.
# mise-2's refusal line ("failed to resolve postgres") never names 99.99.99: the tag appears
# only in progress lines, so the generic gate cannot tie it to the refusal.
EXPECTED = {
    "tilt-1": "prerequisite", "vagrant-1": "prerequisite", "devpod-1": "prerequisite",
    "berth-1": "prerequisite", "branchbox-1": "prerequisite", "mise-2": None,
    **{k: "intended" for k in ("compose-2", "devbox-1", "devcontainers-2", "devenv-2", "dnvr-1", "flox-1",
                               "isola-1", "nix-1", "organist-1", "pixi-2", "pkgx-1", "process-compose-1",
                               "services-flake-1", "stack-6", "worktrunk-1", "workz-1")},
}


def classify(case, stdout=None, stderr=None):
    cls = registry.load(case["tool"])[0]
    return verify.bad_config_evidence(
        (case["stdout"] if stdout is None else stdout, case["stderr"] if stderr is None else stderr),
        cls.bad_config_pattern, cls.bad_config_refusal, cls.bad_config_prerequisite)


def default(stdout="", stderr=""):
    return verify.bad_config_evidence((stdout, stderr), Adapter.bad_config_pattern)


class RawLogProvenance(unittest.TestCase):
    def test_embedded_logs_match_retained_raw_files(self):
        results = BENCH / "results"
        checked = 0
        for key, case in RAW.items():
            for ext in ("stdout", "stderr"):
                path = results / f"{case['path']}.{ext}"
                if not path.exists():
                    continue
                data = path.read_bytes()
                self.assertEqual(hashlib.sha256(data).hexdigest(), case[f"{ext}_sha256"], path)
                embedded = case[ext].encode()
                if case[f"{ext}_tail"]:
                    self.assertTrue(data.endswith(embedded), path)
                else:
                    self.assertEqual(data, embedded, path)
                checked += 1
        if not checked:
            self.skipTest("bench/results not present on this host")


class RetainedLogs(unittest.TestCase):
    def test_every_retained_log_classifies_as_expected(self):
        self.assertEqual(set(EXPECTED), set(RAW))
        for key, case in RAW.items():
            with self.subTest(key):
                self.assertEqual(classify(case)[0], EXPECTED[key])

    def test_tilt_and_vagrant_false_positives_are_credential_prerequisites(self):
        for key in ("tilt-1", "vagrant-1"):
            kind, line = classify(RAW[key])
            self.assertEqual(kind, "prerequisite", key)
            self.assertIn("error getting credentials", line)
            # The old gate's evidence: the tag really is in the refused step's output.
            self.assertTrue(verify.relevant(RAW[key]["stdout"] + "\n" + RAW[key]["stderr"],
                                            Adapter.bad_config_pattern))

    def test_tilt_and_vagrant_tag_lines_alone_are_not_intended(self):
        # Strip the credential lines: what remains (progress, argv, Docker's pre-pull notice,
        # "Build Failed: command [...] failed") must still not be an intended refusal.
        for key in ("tilt-1", "vagrant-1", "devpod-1"):
            case = RAW[key]
            drop = lambda t: "\n".join(l for l in t.splitlines() if "credentials" not in l)
            self.assertEqual(classify(case, drop(case["stdout"]), drop(case["stderr"]))[0], None, key)

    def test_intended_lines_name_the_requested_version(self):
        for key, want in EXPECTED.items():
            if want == "intended":
                cls = registry.load(RAW[key]["tool"])[0]
                line = classify(RAW[key])[1]
                self.assertTrue(verify.relevant(line, cls.bad_config_pattern), (key, line))
                self.assertTrue(verify.relevant(line, cls.bad_config_refusal), (key, line))

    def test_pixi_wrapped_refusal_joins_its_continuation(self):
        self.assertIn("No candidates were found for postgresql 99.99.99", classify(RAW["pixi-2"])[1])

    def test_devenv_ansi_coloured_refusal(self):
        self.assertIn("\x1b[", RAW["devenv-2"]["stderr"])
        self.assertEqual(classify(RAW["devenv-2"])[1], "error: attribute 'postgresql_99' missing")


class Synthetic(unittest.TestCase):
    def test_tag_only_in_progress_or_argv_is_never_intended(self):
        for out in ("Image postgres:99.99.99-alpine Pulling\nError: exit status 1\n",
                    "Unable to find image 'postgres:99.99.99-alpine' locally\nError: context deadline exceeded\n",
                    'Command: ["docker", "run", "postgres:99.99.99-alpine"]\nfailed\n',
                    "-> Processing 5 tool(s): postgres@99.99.99, redis@8.10.2\nERROR failed to resolve redis\n",
                    "pg:  Image: postgres:99.99.99-alpine\nError: boom\n",
                    "postgres:99.99.99-alpine\n",
                    "error: not found\nPulling postgres:99.99.99\n"):
            with self.subTest(out):
                self.assertEqual(default(stderr=out), (None, ""))

    def test_credential_and_network_failures_block_even_when_naming_the_tag(self):
        for out in ("retrieve image postgres:99.99.99-alpine: error getting credentials - err: exit status 1\n",
                    "pull postgres:99.99.99: Get \"https://registry-1.docker.io/v2/\": dial tcp: lookup registry-1.docker.io: no such host\n",
                    "postgres:99.99.99: unauthorized: authentication required\n",
                    "postgres:99.99.99: toomanyrequests: You have reached your pull rate limit\n",
                    "fetch postgres:99.99.99 not found: net/http: TLS handshake timeout\n",
                    "postgres@99.99.99: dial tcp 104.16.0.1:443: connect: connection refused\n",
                    "error: unable to download 'https://cache.nixos.org/x': Couldn't resolve host name; postgresql_99\n",
                    "exec: \"docker-credential-desktop\": executable file not found in $PATH\n"):
            with self.subTest(out):
                self.assertEqual(default(stderr=out)[0], "prerequisite")

    def test_terminal_diagnostic_decides(self):
        refusal = "manifest for postgres:99.99.99-alpine not found: manifest unknown\n"
        prereq = "error getting credentials - err: exit status 1\n"
        self.assertEqual(default(stderr=prereq + refusal)[0], "intended")   # registry answered last
        self.assertEqual(default(stderr=refusal + prereq)[0], "prerequisite")
        # A terminal prerequisite in either stream blocks, whatever the other stream says.
        self.assertEqual(default(stdout=refusal, stderr=prereq)[0], "prerequisite")

    def test_loopback_dial_is_not_a_network_prerequisite(self):
        line = "127.0.0.1:1 (127.0.0.1): dial error: dial tcp 127.0.0.1:1: connect: connection refused\n"
        isola = registry.load("isola")[0]
        self.assertEqual(verify.bad_config_evidence(("", line), isola.bad_config_pattern)[0], "intended")

    def test_continuation_only_for_mid_phrase_wraps(self):
        self.assertEqual(default(stderr="No candidates were found for\n    postgresql 99.99.99.*.\n")[0], "intended")
        self.assertEqual(default(stderr="error: package not found.\n    Pulling postgres:99.99.99\n")[0], None)
        self.assertEqual(default(stderr="No candidates were found for\npostgres:99.99.99 Pulling\n")[0], None)

    def test_complete_diagnostic_never_borrows_tag_from_next_record(self):
        # A diagnostic ending in a word/comma is not an unfinished phrase; the indented line
        # after it is a fresh progress/argv/log record, not its object.
        prereq = "error getting credentials - err: exit status 1\n"
        for out in ("error: redis package not found\n    Pulling postgres:99.99.99\n",
                    "error: package not found,\n    Pulling postgres:99.99.99\n",
                    "No candidates were found for\n    Pulling postgres:99.99.99\n",
                    "No candidates were found for\n    Image postgres:99.99.99-alpine Pulling\n",
                    'No candidates were found for\n    ["docker", "pull", "postgres:99.99.99"]\n',
                    "No candidates were found for\n    2026-10-06T16:10:53Z INFO pull postgres 99.99.99\n",
                    "No candidates were found for\n    INFO postgres 99.99.99\n",
                    "No candidates were found for\n    postgres 99.99.99 Pulling\n"):
            with self.subTest(out):
                self.assertEqual(default(stderr=out), (None, ""))
                # A prior credential failure stays terminal; nothing fabricated overrides it.
                self.assertEqual(default(stderr=prereq + out)[0], "prerequisite")

    def test_pixi_continuation_after_prior_credential_is_a_genuine_retry(self):
        prereq = "error getting credentials - err: exit status 1\n"
        wrap = "No candidates were found for\n      postgresql 99.99.99.*.\n"
        self.assertEqual(default(stderr=prereq + wrap),
                         ("intended", "No candidates were found for postgresql 99.99.99.*."))

    def test_empty_tag_never_matches(self):
        self.assertEqual(verify.bad_config_evidence(("", "not found 99.99.99"), ""), (None, ""))


class Toy(Adapter):
    name, title, image = "toy", "Toy", "ev-base"
    features = {k: "native" for k in FEATURES}
    lock_files = ("toy.lock",)

    def versions(self): return "toy --version"
    def setup(self, co): return f"toy setup {co.name}"
    def frozen_setup(self, co): return f"toy setup --locked {co.name}"
    def enter(self, co, body): return f"toy exec -- {body}"
    def start(self, co): return "toy up"
    def ready(self, co): return "toy wait"
    def stop(self, co): return "toy down"
    def status(self, co): return "toy status"
    def break_config(self, co): return "sed -i s/17/99.99.99/ toy.toml"


def bad_config(start=None, setup=None):
    """Run the fake scenario; setup=(stdout, stderr) refuses at setup, start= refuses at start."""
    rec, world = FakeRecorder(), FakeWorld()
    adapter = Toy()
    tx = FakeTransport(rec, adapter, world)
    sc = Scenario(adapter, tx, rec, repeats=1, warmups=0)
    world.scenario = sc
    if setup is not None:
        world.outputs["d-setup-invalid"] = setup
    if start is not None:
        world.codes["d-setup-invalid"] = (0, False)
        world.codes["d-start-invalid"] = (1, False)
        world.outputs["d-setup-invalid"] = ("", "")
        world.outputs["d-start-invalid"] = start
    sc.execute()
    sc.cleanup()
    return {o["check"]: o for o in sc.out.as_list()}["bad_config"]


class ScenarioGate(unittest.TestCase):
    def test_raw_tilt_and_vagrant_start_refusals_are_blocked_not_pass(self):
        for key in ("tilt-1", "vagrant-1"):
            out = bad_config(start=(RAW[key]["stdout"], RAW[key]["stderr"]))
            self.assertEqual(out["status"], "blocked", key)
            self.assertIn("environment prerequisite", out["detail"], key)
            self.assertIn("error getting credentials", out["detail"], key)

    def test_raw_valid_refusal_still_passes(self):
        out = bad_config(setup=(RAW["compose-2"]["stdout"], RAW["compose-2"]["stderr"]))
        self.assertEqual(out["status"], "pass", out["detail"])
        self.assertIn("intended diagnostic", out["detail"])

    def test_tag_only_progress_refusal_is_blocked(self):
        out = bad_config(start=("Image postgres:99.99.99-alpine Pulling\n", "Error: exit status 1\n"))
        self.assertEqual(out["status"], "blocked")
        self.assertIn("lacks the intended diagnostic", out["detail"])

    def test_unrelated_diagnostic_plus_indented_progress_is_blocked(self):
        out = "error: redis package not found\n    Pulling postgres:99.99.99\n"
        for prefix in ("", "error getting credentials - err: exit status 1\n"):
            with self.subTest(prefix=prefix):
                res = bad_config(setup=("", prefix + out))
                self.assertEqual(res["status"], "blocked", res["detail"])
                self.assertNotIn("(intended diagnostic)", res["detail"])
                if prefix:
                    self.assertIn("environment prerequisite", res["detail"])
                    self.assertIn("error getting credentials", res["detail"])
                else:
                    self.assertIn("lacks the intended diagnostic", res["detail"])

    def test_raw_pixi_wrapped_refusal_still_passes(self):
        res = bad_config(setup=(RAW["pixi-2"]["stdout"], RAW["pixi-2"]["stderr"]))
        self.assertEqual(res["status"], "pass", res["detail"])
        self.assertIn("intended diagnostic", res["detail"])


if __name__ == "__main__":
    unittest.main()
