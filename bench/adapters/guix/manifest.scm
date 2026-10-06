;; Exact package versions at the pinned channel commit (bench/research/guix.md).
;; NOT the canonical PostgreSQL 17 / Redis 8 workload: this Guix revision provides
;; PostgreSQL 16.14 and Redis 7.2.6. The deviation is reported, never relabelled.
;; bash/coreutils/sed/grep serve `guix shell --pure` and the shared service script.
(use-modules (guix profiles) (gnu packages))
(specifications->manifest
 '("python@3.13.13" "postgresql@16.14" "redis@7.2.6" "uv@0.10.12"
   "bash" "coreutils" "sed" "grep" "nss-certs"))
