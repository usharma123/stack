# Guix 1.5.0 ARM64 release verification

Verified on 2026-10-06 before benchmark provisioning. This is download provenance,
not evidence that Guix builds or the benchmark workload work in this environment.

- Archive: https://mirrors.kernel.org/gnu/guix/guix-binary-1.5.0.aarch64-linux.tar.xz
- Signature: same URL with `.sig` appended (retained here).
- Archive SHA-256: `a5d58b1d0294cad6adb1f2aff627d37feb5db763fdffbceb8551f2b12123cf39`
- Signing fingerprint: `A28BF40C3E551372662D14F741AAE7DCCA3D8351`.
- Fingerprint source: Guix `etc/guix-install.sh`, `GPG_SIGNING_KEYS["efraim"]`,
  at source revision `71d010188f039817c465985e46e185445fda6946`.
- Public key fetched from https://keys.openpgp.org/vks/v1/by-fingerprint/A28BF40C3E551372662D14F741AAE7DCCA3D8351
  and imported into a task-private GPG directory. The exported server response is retained.
- `verification.txt` records GPG exit 0 and `VALIDSIG` for the pinned fingerprint.
  GPG's trust warning refers to the empty private trust database; the fingerprint
  was checked against the upstream installer. The log also contains `KEYEXPIRED`
  metadata; GPG reports a good signature and does not report an expired signing signature.

The 135 MB archive is not committed. Supply these options to the Guix adapter:

```text
--option guix_binary_url=https://mirrors.kernel.org/gnu/guix/guix-binary-1.5.0.aarch64-linux.tar.xz
--option guix_binary_sha256=a5d58b1d0294cad6adb1f2aff627d37feb5db763fdffbceb8551f2b12123cf39
```

This resolves the earlier GNU server download timeout. The normal sandbox canary
must still run; no relaxed daemon flags were used for this verification.
