# Measurement report final-20261006-reviewed-1

Manifest `/Users/utsavsharma/.t3/projects/stack/bench/measurements/final-20261006-reviewed-1/manifest.json` sha256 `6142bd038a6de60b1c603c0ffd841c0391a4b3ad34515a8e0e84b51c615f6ab2`.
Plan created 2026-10-06T17:19:10Z · samples 20 + 3 warmups · harness `ee96b465dc30d6a59933c6d99b499f55194104fc`.
Session review: bench/reviews/astra-final-results-r1.md.
Serialization: Parent attests no concurrent benchmark, build or install workers while either executor was active. There were 27 serial attempts: 15 completed attempts, one interrupted dnvr attempt, then 11 completed attempts starting with the dnvr retry after the sealed resume supplement. The interrupted owned container was removed before retry. There is no claim of uninterrupted execution or idle time across the interruption. Unrelated preexisting services remained running.

Coverage complete: True

| entry | tool:variant | coverage | outcomes | reported metrics |
|---|---|---|---|---|
| Stack | stack:default | completed | fail 1, observed 2, pass 26 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| mise / Pitchfork | mise:default | completed | blocked 1, observed 2, pass 26 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Flox | flox:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Devbox | devbox:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| devenv | devenv:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Nix | nix:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Pixi | pixi:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Docker Compose | compose:default | completed | not_applicable 1, observed 2, pass 26 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Dev Containers | devcontainers:default | completed | not_applicable 1, observed 2, pass 26 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| DevPod | devpod:default | completed | blocked 1, not_applicable 1, observed 2, pass 25 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| DDEV | ddev:default | completed | fail 1, observed 1, pass 1 | - |
| Lando | lando:default | completed | not_applicable 1, observed 2, pass 26 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Process Compose | process-compose:default | completed | fail 1, observed 2, pass 26 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| services-flake | services-flake:default | completed | fail 1, observed 2, pass 26 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| pkgx / dev | pkgx:default | completed | observed 2, pass 25, unsupported 2 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| dnvr | dnvr:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| GNU Guix | guix:default | blocked (provisioning): provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used | blocked 28, observed 1, pass 1 | - |
| workz (rohansx) | workz:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Worktrunk | worktrunk:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| GitGrove | git-grove:default | completed | blocked 1, observed 2, pass 26 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| isola | isola:default | completed | not_applicable 1, observed 3, pass 24, unsupported 2 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Berth | berth:default | completed | blocked 1, not_applicable 1, observed 2, pass 23, unsupported 2 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| BranchBox | branchbox:default | completed | blocked 1, not_applicable 1, observed 1, pass 23, unsupported 3 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Tilt | tilt:default | completed | blocked 1, not_applicable 1, observed 2, pass 23, unsupported 2 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Organist | organist:default | completed | observed 2, pass 27 | first_task.a, first_task.b, repeat.entry, repeat.app_read |
| Vagrant | vagrant:default | completed | blocked 1, not_applicable 1, observed 2, pass 23, unsupported 2 | first_task.a, first_task.b, repeat.entry, repeat.app_read |

## Reported timings

`first_task.*`: one observation (sum of task step receipts, from a prepared checkout). `repeat.*`: nearest-rank p50/p95 over one run's samples; warmups excluded.

| tool | run | metric | value | n | inner available | evidence |
|---|---|---|---|---|---|---|
| stack | 20261006t171921-c8eb41 | first_task.a | 11.048 s (wall 11.466 s) | 1 | - | seq 7-19 |
| stack | 20261006t171921-c8eb41 | first_task.b | 3.636 s (wall 3.898 s) | 1 | - | seq 23-34 |
| stack | 20261006t171921-c8eb41 | repeat.entry | outer p50 87.497 / p95 113.021 ms | 20 | 20/20 (p50 29.861 ms) | seq 40-59 |
| stack | 20261006t171921-c8eb41 | repeat.app_read | outer p50 205.405 / p95 238.397 ms | 20 | 20/20 (p50 149.071 ms) | seq 63-82 |
| mise | 20261006t171958-c79567 | first_task.a | 10.405 s (wall 10.699 s) | 1 | - | seq 6-17 |
| mise | 20261006t171958-c79567 | first_task.b | 3.573 s (wall 3.827 s) | 1 | - | seq 21-31 |
| mise | 20261006t171958-c79567 | repeat.entry | outer p50 73.548 / p95 111.284 ms | 20 | 20/20 (p50 9.726 ms) | seq 37-56 |
| mise | 20261006t171958-c79567 | repeat.app_read | outer p50 195.845 / p95 255.729 ms | 20 | 20/20 (p50 132.651 ms) | seq 60-79 |
| flox | 20261006t172032-28a7b7 | first_task.a | 35.672 s (wall 36.128 s) | 1 | - | seq 6-17 |
| flox | 20261006t172032-28a7b7 | first_task.b | 6.046 s (wall 6.352 s) | 1 | - | seq 21-31 |
| flox | 20261006t172032-28a7b7 | repeat.entry | outer p50 74.993 / p95 97.757 ms | 20 | 20/20 (p50 18.529 ms) | seq 37-56 |
| flox | 20261006t172032-28a7b7 | repeat.app_read | outer p50 210.869 / p95 247.051 ms | 20 | 20/20 (p50 149.775 ms) | seq 60-79 |
| devbox | 20261006t172345-ef5197 | first_task.a | 71.174 s (wall 71.865 s) | 1 | - | seq 7-18 |
| devbox | 20261006t172345-ef5197 | first_task.b | 6.984 s (wall 7.671 s) | 1 | - | seq 22-32 |
| devbox | 20261006t172345-ef5197 | repeat.entry | outer p50 117.768 / p95 286.232 ms | 20 | 20/20 (p50 53.055 ms) | seq 38-57 |
| devbox | 20261006t172345-ef5197 | repeat.app_read | outer p50 238.773 / p95 298.21 ms | 20 | 20/20 (p50 175.559 ms) | seq 61-80 |
| devenv | 20261006t172749-4fe69b | first_task.a | 25.276 s (wall 25.68 s) | 1 | - | seq 7-19 |
| devenv | 20261006t172749-4fe69b | first_task.b | 13.265 s (wall 13.61 s) | 1 | - | seq 23-34 |
| devenv | 20261006t172749-4fe69b | repeat.entry | outer p50 101.634 / p95 110.878 ms | 20 | 20/20 (p50 47.585 ms) | seq 40-59 |
| devenv | 20261006t172749-4fe69b | repeat.app_read | outer p50 235.787 / p95 367.231 ms | 20 | 20/20 (p50 177.867 ms) | seq 63-82 |
| nix | 20261006t172944-ff0b3e | first_task.a | 31.839 s (wall 34.277 s) | 1 | - | seq 6-17 |
| nix | 20261006t172944-ff0b3e | first_task.b | 13.22 s (wall 15.485 s) | 1 | - | seq 21-31 |
| nix | 20261006t172944-ff0b3e | repeat.entry | outer p50 866.5 / p95 1084.716 ms | 20 | 20/20 (p50 801.025 ms) | seq 37-56 |
| nix | 20261006t172944-ff0b3e | repeat.app_read | outer p50 965.862 / p95 1094.007 ms | 20 | 20/20 (p50 891.462 ms) | seq 60-79 |
| pixi | 20261006t173145-8fe768 | first_task.a | 8.736 s (wall 9.121 s) | 1 | - | seq 6-17 |
| pixi | 20261006t173145-8fe768 | first_task.b | 4.176 s (wall 4.487 s) | 1 | - | seq 21-31 |
| pixi | 20261006t173145-8fe768 | repeat.entry | outer p50 73.573 / p95 81.927 ms | 20 | 20/20 (p50 16.328 ms) | seq 37-56 |
| pixi | 20261006t173145-8fe768 | repeat.app_read | outer p50 179.013 / p95 187.914 ms | 20 | 20/20 (p50 120.056 ms) | seq 60-79 |
| compose | 20261006t173215-62be8d | first_task.a | 18.752 s (wall 21.17 s) | 1 | - | seq 4-16 |
| compose | 20261006t173215-62be8d | first_task.b | 14.929 s (wall 17.369 s) | 1 | - | seq 21-32 |
| compose | 20261006t173215-62be8d | repeat.entry | outer p50 71.296 / p95 82.307 ms | 20 | 0/20 (p50 None ms) | seq 38-57 |
| compose | 20261006t173215-62be8d | repeat.app_read | outer p50 628.934 / p95 676.348 ms | 20 | 0/20 (p50 None ms) | seq 61-80 |
| devcontainers | 20261006t173334-55bcb5 | first_task.a | 14.229 s (wall 15.715 s) | 1 | - | seq 5-17 |
| devcontainers | 20261006t173334-55bcb5 | first_task.b | 13.242 s (wall 14.719 s) | 1 | - | seq 22-33 |
| devcontainers | 20261006t173334-55bcb5 | repeat.entry | outer p50 355.302 / p95 387.364 ms | 20 | 0/20 (p50 None ms) | seq 39-58 |
| devcontainers | 20261006t173334-55bcb5 | repeat.app_read | outer p50 947.126 / p95 1081.168 ms | 20 | 0/20 (p50 None ms) | seq 62-81 |
| devpod | 20261006t173510-1b1b4e | first_task.a | 16.83 s (wall 19.191 s) | 1 | - | seq 6-18 |
| devpod | 20261006t173510-1b1b4e | first_task.b | 16.945 s (wall 19.414 s) | 1 | - | seq 23-34 |
| devpod | 20261006t173510-1b1b4e | repeat.entry | outer p50 781.406 / p95 1010.087 ms | 20 | 0/20 (p50 None ms) | seq 40-59 |
| devpod | 20261006t173510-1b1b4e | repeat.app_read | outer p50 1436.507 / p95 1674.465 ms | 20 | 0/20 (p50 None ms) | seq 63-82 |
| lando | 20261006t173725-3e750a | first_task.a | 18.807 s (wall 20.632 s) | 1 | - | seq 7-19 |
| lando | 20261006t173725-3e750a | first_task.b | 17.497 s (wall 19.185 s) | 1 | - | seq 24-35 |
| lando | 20261006t173725-3e750a | repeat.entry | outer p50 291.895 / p95 312.633 ms | 20 | 0/20 (p50 None ms) | seq 41-60 |
| lando | 20261006t173725-3e750a | repeat.app_read | outer p50 871.643 / p95 911.778 ms | 20 | 0/20 (p50 None ms) | seq 64-83 |
| process-compose | 20261006t173929-c70737 | first_task.a | 29.924 s (wall 32.283 s) | 1 | - | seq 6-18 |
| process-compose | 20261006t173929-c70737 | first_task.b | 13.025 s (wall 14.954 s) | 1 | - | seq 22-33 |
| process-compose | 20261006t173929-c70737 | repeat.entry | outer p50 879.626 / p95 962.429 ms | 20 | 20/20 (p50 819.452 ms) | seq 39-58 |
| process-compose | 20261006t173929-c70737 | repeat.app_read | outer p50 1018.763 / p95 1486.801 ms | 20 | 20/20 (p50 958.076 ms) | seq 62-81 |
| services-flake | 20261006t174332-f5a4e8 | first_task.a | 33.151 s (wall 35.487 s) | 1 | - | seq 5-17 |
| services-flake | 20261006t174332-f5a4e8 | first_task.b | 15.59 s (wall 18.283 s) | 1 | - | seq 21-32 |
| services-flake | 20261006t174332-f5a4e8 | repeat.entry | outer p50 898.312 / p95 1008.007 ms | 20 | 20/20 (p50 836.036 ms) | seq 38-57 |
| services-flake | 20261006t174332-f5a4e8 | repeat.app_read | outer p50 988.412 / p95 1061.521 ms | 20 | 20/20 (p50 914.1 ms) | seq 61-80 |
| pkgx | 20261006t174747-157710 | first_task.a | 11.785 s (wall 12.202 s) | 1 | - | seq 7-17 |
| pkgx | 20261006t174747-157710 | first_task.b | 3.66 s (wall 4.081 s) | 1 | - | seq 21-31 |
| pkgx | 20261006t174747-157710 | repeat.entry | outer p50 136.772 / p95 144.844 ms | 20 | 20/20 (p50 71.301 ms) | seq 37-56 |
| pkgx | 20261006t174747-157710 | repeat.app_read | outer p50 260.317 / p95 350.973 ms | 20 | 20/20 (p50 199.312 ms) | seq 60-79 |
| dnvr | 20261006t175437-f511c6 | first_task.a | 50.034 s (wall 52.262 s) | 1 | - | seq 5-16 |
| dnvr | 20261006t175437-f511c6 | first_task.b | 15.168 s (wall 17.148 s) | 1 | - | seq 20-30 |
| dnvr | 20261006t175437-f511c6 | repeat.entry | outer p50 950.476 / p95 1043.441 ms | 20 | 20/20 (p50 878.899 ms) | seq 36-55 |
| dnvr | 20261006t175437-f511c6 | repeat.app_read | outer p50 976.824 / p95 1089.532 ms | 20 | 20/20 (p50 912.757 ms) | seq 59-78 |
| workz | 20261006t175943-16c496 | first_task.a | 8.362 s (wall 9.093 s) | 1 | - | seq 5-18 |
| workz | 20261006t175943-16c496 | first_task.b | 5.984 s (wall 6.48 s) | 1 | - | seq 24-36 |
| workz | 20261006t175943-16c496 | repeat.entry | outer p50 21.108 / p95 23.113 ms | 20 | 0/20 (p50 None ms) | seq 42-61 |
| workz | 20261006t175943-16c496 | repeat.app_read | outer p50 181.461 / p95 203.449 ms | 20 | 0/20 (p50 None ms) | seq 65-84 |
| worktrunk | 20261006t180026-572cdd | first_task.a | 7.555 s (wall 8.151 s) | 1 | - | seq 5-18 |
| worktrunk | 20261006t180026-572cdd | first_task.b | 7.475 s (wall 7.992 s) | 1 | - | seq 24-36 |
| worktrunk | 20261006t180026-572cdd | repeat.entry | outer p50 28.654 / p95 31.336 ms | 20 | 0/20 (p50 None ms) | seq 42-61 |
| worktrunk | 20261006t180026-572cdd | repeat.app_read | outer p50 180.093 / p95 184.15 ms | 20 | 0/20 (p50 None ms) | seq 65-84 |
| git-grove | 20261006t180109-89ad03 | first_task.a | 9.662 s (wall 10.531 s) | 1 | - | seq 5-17 |
| git-grove | 20261006t180109-89ad03 | first_task.b | 8.132 s (wall 8.923 s) | 1 | - | seq 22-33 |
| git-grove | 20261006t180109-89ad03 | repeat.entry | outer p50 109.905 / p95 120.493 ms | 20 | 0/20 (p50 None ms) | seq 39-58 |
| git-grove | 20261006t180109-89ad03 | repeat.app_read | outer p50 637.6 / p95 681.99 ms | 20 | 0/20 (p50 None ms) | seq 62-81 |
| isola | 20261006t180223-37caf7 | first_task.a | 9.469 s (wall 9.93 s) | 1 | - | seq 7-18 |
| isola | 20261006t180223-37caf7 | first_task.b | 2.166 s (wall 2.513 s) | 1 | - | seq 23-34 |
| isola | 20261006t180223-37caf7 | repeat.entry | outer p50 64.35 / p95 72.939 ms | 20 | 20/20 (p50 4.992 ms) | seq 40-59 |
| isola | 20261006t180223-37caf7 | repeat.app_read | outer p50 172.651 / p95 194.172 ms | 20 | 20/20 (p50 112.473 ms) | seq 63-82 |
| berth | 20261006t180256-9d10d9 | first_task.a | 11.786 s (wall 13.363 s) | 1 | - | seq 4-15 |
| berth | 20261006t180256-9d10d9 | first_task.b | 9.22 s (wall 10.987 s) | 1 | - | seq 20-31 |
| berth | 20261006t180256-9d10d9 | repeat.entry | outer p50 110.024 / p95 121.03 ms | 20 | 0/20 (p50 None ms) | seq 37-56 |
| berth | 20261006t180256-9d10d9 | repeat.app_read | outer p50 587.888 / p95 641.14 ms | 20 | 0/20 (p50 None ms) | seq 60-79 |
| branchbox | 20261006t180409-b89259 | first_task.a | 8.804 s (wall 10.542 s) | 1 | - | seq 4-15 |
| branchbox | 20261006t180409-b89259 | first_task.b | 7.005 s (wall 8.775 s) | 1 | - | seq 20-31 |
| branchbox | 20261006t180409-b89259 | repeat.entry | outer p50 161.961 / p95 174.607 ms | 20 | 0/20 (p50 None ms) | seq 37-56 |
| branchbox | 20261006t180409-b89259 | repeat.app_read | outer p50 624.765 / p95 653.074 ms | 20 | 0/20 (p50 None ms) | seq 60-79 |
| tilt | 20261006t180504-a16963 | first_task.a | 10.08 s (wall 10.594 s) | 1 | - | seq 4-16 |
| tilt | 20261006t180504-a16963 | first_task.b | 8.753 s (wall 9.296 s) | 1 | - | seq 21-33 |
| tilt | 20261006t180504-a16963 | repeat.entry | outer p50 67.249 / p95 73.848 ms | 20 | 0/20 (p50 None ms) | seq 39-58 |
| tilt | 20261006t180504-a16963 | repeat.app_read | outer p50 254.296 / p95 358.355 ms | 20 | 0/20 (p50 None ms) | seq 62-81 |
| organist | 20261006t180558-6df6a6 | first_task.a | 52.36 s (wall 55.611 s) | 1 | - | seq 6-17 |
| organist | 20261006t180558-6df6a6 | first_task.b | 16.377 s (wall 18.994 s) | 1 | - | seq 21-31 |
| organist | 20261006t180558-6df6a6 | repeat.entry | outer p50 1132.498 / p95 1226.317 ms | 20 | 20/20 (p50 1075.123 ms) | seq 37-56 |
| organist | 20261006t180558-6df6a6 | repeat.app_read | outer p50 1308.313 / p95 1377.731 ms | 20 | 20/20 (p50 1250.357 ms) | seq 60-79 |
| vagrant | 20261006t180848-312625 | first_task.a | 35.971 s (wall 47.791 s) | 1 | - | seq 4-15 |
| vagrant | 20261006t180848-312625 | first_task.b | 30.478 s (wall 39.423 s) | 1 | - | seq 20-31 |
| vagrant | 20261006t180848-312625 | repeat.entry | outer p50 2456.605 / p95 2667.259 ms | 20 | 0/20 (p50 None ms) | seq 37-56 |
| vagrant | 20261006t180848-312625 | repeat.app_read | outer p50 2516.719 / p95 3115.045 ms | 20 | 0/20 (p50 None ms) | seq 60-79 |

## Omitted metrics

| tool | metric | reason |
|---|---|---|
| ddev | first_task.a | run not eligible: incomplete outcomes: missing lock.created, deps.a, start.a, migrate.a, crud_cache.a, tests.a, start.repeat, setup.b, deps.b, start.b, migrate.b, crud_cache.b, tests.b, isolation, repeat.entry, repeat.app_read, status, stop.a, b.survives, restart.a, persist.pg, persist.redis, cache. |
| ddev | first_task.b | run not eligible: incomplete outcomes: missing lock.created, deps.a, start.a, migrate.a, crud_cache.a, tests.a, start.repeat, setup.b, deps.b, start.b, migrate.b, crud_cache.b, tests.b, isolation, repeat.entry, repeat.app_read, status, stop.a, b.survives, restart.a, persist.pg, persist.redis, cache. |
| ddev | repeat.entry | run not eligible: incomplete outcomes: missing lock.created, deps.a, start.a, migrate.a, crud_cache.a, tests.a, start.repeat, setup.b, deps.b, start.b, migrate.b, crud_cache.b, tests.b, isolation, repeat.entry, repeat.app_read, status, stop.a, b.survives, restart.a, persist.pg, persist.redis, cache. |
| ddev | repeat.app_read | run not eligible: incomplete outcomes: missing lock.created, deps.a, start.a, migrate.a, crud_cache.a, tests.a, start.repeat, setup.b, deps.b, start.b, migrate.b, crud_cache.b, tests.b, isolation, repeat.entry, repeat.app_read, status, stop.a, b.survives, restart.a, persist.pg, persist.redis, cache. |
| guix | first_task.a | run not eligible: provisioning blocked: provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; no single s |
| guix | first_task.b | run not eligible: provisioning blocked: provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; no single s |
| guix | repeat.entry | run not eligible: provisioning blocked: provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; no single s |
| guix | repeat.app_read | run not eligible: provisioning blocked: provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; no single s |

## Attempts

- stack:default selected `bench/results/final-20261006-reviewed-1/stack` run 20261006t171921-c8eb41 · review bench/reviews/astra-final-results-r1.md · completed
- mise:default selected `bench/results/final-20261006-reviewed-1/mise` run 20261006t171958-c79567 · review bench/reviews/astra-final-results-r1.md · completed
- flox:default selected `bench/results/final-20261006-reviewed-1/flox` run 20261006t172032-28a7b7 · review bench/reviews/astra-final-results-r1.md · completed
- devbox:default selected `bench/results/final-20261006-reviewed-1/devbox` run 20261006t172345-ef5197 · review bench/reviews/astra-final-results-r1.md · completed
- devenv:default selected `bench/results/final-20261006-reviewed-1/devenv` run 20261006t172749-4fe69b · review bench/reviews/astra-final-results-r1.md · completed
- nix:default selected `bench/results/final-20261006-reviewed-1/nix` run 20261006t172944-ff0b3e · review bench/reviews/astra-final-results-r1.md · completed
- pixi:default selected `bench/results/final-20261006-reviewed-1/pixi` run 20261006t173145-8fe768 · review bench/reviews/astra-final-results-r1.md · completed
- compose:default selected `bench/results/final-20261006-reviewed-1/compose` run 20261006t173215-62be8d · review bench/reviews/astra-final-results-r1.md · completed
- devcontainers:default selected `bench/results/final-20261006-reviewed-1/devcontainers` run 20261006t173334-55bcb5 · review bench/reviews/astra-final-results-r1.md · completed
- devpod:default selected `bench/results/final-20261006-reviewed-1/devpod` run 20261006t173510-1b1b4e · review bench/reviews/astra-final-results-r1.md · completed
- ddev:default selected `bench/results/final-20261006-reviewed-1/ddev` run 20261006t173718-c83372 · review bench/reviews/astra-final-results-r1.md · completed · Frozen adapter tries download-images pull of not-yet-built local app image; no normal DDEV startup performance or product failure inference.
- lando:default selected `bench/results/final-20261006-reviewed-1/lando` run 20261006t173725-3e750a · review bench/reviews/astra-final-results-r1.md · completed
- process-compose:default selected `bench/results/final-20261006-reviewed-1/process-compose` run 20261006t173929-c70737 · review bench/reviews/astra-final-results-r1.md · completed
- services-flake:default selected `bench/results/final-20261006-reviewed-1/services-flake` run 20261006t174332-f5a4e8 · review bench/reviews/astra-final-results-r1.md · completed
- pkgx:default selected `bench/results/final-20261006-reviewed-1/pkgx` run 20261006t174747-157710 · review bench/reviews/astra-final-results-r1.md · completed
- dnvr:default excluded `bench/results/final-20261006-reviewed-1/dnvr` run 20261006t174831-ed019a · review bench/reviews/astra-final-results-r1.md · evidence missing: missing outcomes.json; unparseable evidence: [Errno 2] No such file or directory: '/Users/utsavsharma/.t3/projects/stack/bench/results/final-20261006-reviewed-1/dnvr/outcomes.json' · Prior task cancelled during repeat.app_read after sample 009. Executor absent on fresh resumption snapshot; completed/outcomes/final interval absent. Partial raw receipts preserved unchanged. Exact owned container captured and removed after fresh identity confirmation; dnvr retried at dnvr-retry-1. Never timing eligible.
- dnvr:default selected `bench/results/final-20261006-reviewed-1/dnvr-retry-1` run 20261006t175437-f511c6 · review bench/reviews/astra-final-results-r1.md · completed
- guix:default selected `bench/results/final-20261006-reviewed-1/guix` run 20261006t175917-e3a5f2 · review bench/reviews/astra-final-results-r1.md · blocked (provisioning): provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used · provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used
- workz:default selected `bench/results/final-20261006-reviewed-1/workz` run 20261006t175943-16c496 · review bench/reviews/astra-final-results-r1.md · completed
- worktrunk:default selected `bench/results/final-20261006-reviewed-1/worktrunk` run 20261006t180026-572cdd · review bench/reviews/astra-final-results-r1.md · completed
- git-grove:default selected `bench/results/final-20261006-reviewed-1/git-grove` run 20261006t180109-89ad03 · review bench/reviews/astra-final-results-r1.md · completed
- isola:default selected `bench/results/final-20261006-reviewed-1/isola` run 20261006t180223-37caf7 · review bench/reviews/astra-final-results-r1.md · completed
- berth:default selected `bench/results/final-20261006-reviewed-1/berth` run 20261006t180256-9d10d9 · review bench/reviews/astra-final-results-r1.md · completed
- branchbox:default selected `bench/results/final-20261006-reviewed-1/branchbox` run 20261006t180409-b89259 · review bench/reviews/astra-final-results-r1.md · completed
- tilt:default selected `bench/results/final-20261006-reviewed-1/tilt` run 20261006t180504-a16963 · review bench/reviews/astra-final-results-r1.md · completed
- organist:default selected `bench/results/final-20261006-reviewed-1/organist` run 20261006t180558-6df6a6 · review bench/reviews/astra-final-results-r1.md · completed
- vagrant:default selected `bench/results/final-20261006-reviewed-1/vagrant` run 20261006t180848-312625 · review bench/reviews/astra-final-results-r1.md · completed

## Limits

- Describes this host, image/cache state, transport and recipe only; not native macOS results.
- first_task.<co> is one observation per run, not a startup distribution.
- repeat.* p50/p95 are descriptive nearest-rank values over one run's samples; warmups excluded.
- No overall rank, score, confidence claim or universal cold-install claim.
- Interval checks prove selected runs did not overlap each other; the parent declaration, not this report, attests that no other benchmark, build or install work ran concurrently.
